import test from 'node:test';
import assert from 'node:assert/strict';
import {createAssistantMessageEventStream, getCurrentTools, getCurrentSystemPrompt} from '@earendil-works/pi-ai';
import {createSerialAgent} from '../src/pi-agent.mjs';
import {CORE_TOOL_NAMES, toolGroupCatalog} from '../src/agent-tool-groups.mjs';
import {scenarioFixture} from '../eval/agent-scenarios.mjs';

const call = (name, args = {}) => ({type:'toolCall', id:crypto.randomUUID(), name, arguments:args});
const done = [{type:'text', text:'Done; no broader verification claimed.'}];
const config = {endpoint:'https://fixture.invalid/v1', model:'fixture'};
const names = context => getCurrentTools(context.messages).map(tool => tool.name);
const last = context => context.messages.findLast(message => message.role === 'toolResult');
function stream(respond) {
  return (model, context) => {
    const content = respond(context);
    const message = {role:'assistant', api:model.api, provider:model.provider, model:model.id,
      timestamp:Date.now(), content, stopReason:content.some(p=>p.type==='toolCall')?'toolUse':'stop',
      usage:{input:0,output:0,totalTokens:0,cacheRead:0,cacheWrite:0,cost:{input:0,output:0,total:0,cacheRead:0,cacheWrite:0}}};
    const output = createAssistantMessageEventStream();
    output.push({type:'done', reason:message.stopReason, message}); output.end(message);
    return output;
  };
}
const device = () => ({mode:'auto',getStatus:()=>({connected:true,sessionId:1}),readLog:()=>({text:'',cursor:0}),
  execute:()=>assert.fail('Unexpected UART input')});

test('focused default declares only core tools and reduces fixed context by at least 35%', async () => {
  const metrics = {}, lists = {};
  for (const toolProfile of ['full','focused']) {
    const agent = createSerialAgent({config,device:device(),toolProfile,accessory:{},notes:{},
      onEvent:event=>{if(event.type==='context_metrics')metrics[toolProfile]=event;},
      stream:stream(context=>{lists[toolProfile]=names(context); return done;})});
    await agent.prompt('Read-only diagnosis');
  }
  // Interactive choice is capability-gated: headless hosts cannot answer it.
  assert.deepEqual(new Set(lists.focused), new Set([...CORE_TOOL_NAMES].filter(name => name !== "ask_user")));
  assert(metrics.focused.fixedChars < metrics.full.fixedChars * 0.65, JSON.stringify(metrics));
  assert.equal(metrics.focused.historyBudgetChars, 24000);
  assert.equal(metrics.full.toolCount, 27);
  for (const name of lists.full) assert(CORE_TOOL_NAMES.has(name) || toolGroupCatalog(lists.full).some(group=>group.tools.includes(name)), name);
});

test('a group adds real schemas on the next model turn, with no UART input', async () => {
  let round=0, reads=0;
  const agent=createSerialAgent({config,device:device(),webReader:async()=>{reads++;return {text:'Public board manual',untrusted:true};},
    stream:stream(context=>{
      round++;
      if(round===1) {
        assert(!names(context).includes('read_web_page'));
        assert.match(getCurrentSystemPrompt(context.messages),/web:.*read_web_page/);
        return [call('load_tools',{groups:['web']})];
      }
      if(round===2) {
        assert(names(context).includes('read_web_page'));
        assert(!names(context).includes('download_to_target'));
        assert.equal(JSON.parse(last(context).content[0].text).grantsPermission,false);
        return [call('read_web_page',{url:'https://docs.example.org'})];
      }
      assert.match(last(context).content[0].text,/Public board manual/); return done;
    })});
  await agent.prompt('Read documentation'); assert.equal(reads,1); assert.equal(round,3);
});

test('unloaded tool calls and unavailable groups are refused without side effects', async () => {
  let round=0;
  const agent=createSerialAgent({config,device:device(),webReader:()=>assert.fail('Unloaded tool executed'),
    stream:stream(context=>{
      if(++round===1)return [call('read_web_page',{url:'https://docs.example.org'})];
      assert.equal(last(context).isError,true);
      if(round===2)return [call('load_tools',{groups:['accessory']})];
      assert(!names(context).includes('set_wifi')); return done;
    })});
  await agent.prompt('Inspect'); assert.equal(round,3);
});

test('optional actions in the same batch as discovery wait for a later model turn', async () => {
  let round=0,reads=0;
  const agent=createSerialAgent({config,device:device(),webReader:()=>{reads++;return {text:'Manual'};},
    stream:stream(context=>{
      if(++round===1)return [call('load_tools',{groups:['web']}),call('read_web_page',{url:'https://docs.example.org'})];
      assert.equal(reads,0);assert.equal(last(context).isError,true);
      assert(names(context).includes('read_web_page'));return done;
    })});
  await agent.prompt('Read documentation');
});

test('loading files alone cannot bypass the existing capability-probe gate', async () => {
  let round=0;
  const agent=createSerialAgent({config,device:device(),stream:stream(context=>{
    if(++round===1)return [call('load_tools',{groups:['files']})];
    assert(!names(context).includes('read_target_file'));
    if(round===2)return [call('read_target_file',{path:'/etc/os-release'})];
    assert.equal(last(context).isError,true);return done;
  })});
  await agent.prompt('Read a file');
});

test('a profile probe does not announce unloaded tools, and later loading still works', async () => {
  let round=0;
  const target={...device(),getStatus:()=>({connected:true,sessionId:1,profile:{tools:['dd','base64']}})};
  const agent=createSerialAgent({config,device:target,stream:stream(context=>{
    if(++round===1)return [call('get_device_status')];
    if(round===2) {
      assert(!names(context).includes('read_target_file'));
      assert(!JSON.parse(last(context).content[0].text).addedTools?.includes('read_target_file'));
      return [call('load_tools',{groups:['files']})];
    }
    assert(names(context).includes('read_target_file'));return done;
  })});
  await agent.prompt('Inspect capabilities');
});

test('loading an accessory group preserves the management-controller refusal', async () => {
  let round=0,requests=0;
  const agent=createSerialAgent({config,device:device(),accessory:{capability:()=>({available:true}),
    change:async()=>{requests++;throw new Error('User rejected this change');}},stream:stream(context=>{
      if(++round===1)return [call('load_tools',{groups:['accessory']})];
      if(round===2)return [call('set_uart_config',{baud:9600})];
      assert.equal(last(context).isError,true);assert.match(last(context).content[0].text,/rejected/);return done;
    })});
  await agent.prompt('Change UART after approval');assert.equal(requests,1);
});

test('loading verification preserves the current-evidence completion gate', async () => {
  let round=0;
  const agent=createSerialAgent({config,device:device(),stream:stream(context=>{
    if(++round===1)return [call('load_tools',{groups:['verification']})];
    if(round===2)return [call('update_task_plan',{steps:[{title:'Repair',kind:'action',status:'completed',
      expectedResult:'Works',verification:'Assumed',evidenceIds:['invented'],nextAction:''} ]})];
    assert.equal(last(context).isError,true);assert.match(last(context).content[0].text,/Historical or invented/);return done;
  })});
  await agent.prompt('Check only');assert.equal(agent.checkpoint().plan.length,0);
});

test('groups reset per question and saved declarations cannot enable old tools', async () => {
  let round=0;
  const agent=createSerialAgent({config,device:device(),stream:stream(context=>{
    if(++round===1)return [call('load_tools',{groups:['web']})];
    if(round===2)assert(names(context).includes('read_web_page'));
    else assert(!names(context).includes('read_web_page'));
    return done;
  })});
  await agent.prompt('Documentation');
  const saved=agent.snapshot();
  await agent.prompt('Read logs only');
  const restored=createSerialAgent({config,device:device(),restoredMessages:saved,stream:stream(context=>{
    assert(!names(context).includes('read_web_page'));
    assert.equal(context.messages.filter(m=>m.role==='system' && m.content).length,1);
    return done;
  })});
  await restored.prompt('Continue observation only');
});

test('loading tools does not release recovery or serial input review gates', async () => {
  const fixture=await scenarioFixture('unknown-after-reload');let round=0;
  const agent=createSerialAgent({config,device:fixture.device,stream:stream(context=>{
    if(++round===1)return [call('load_tools',{groups:['verification']})];
    if(round===2)return [call('verify_target_file',{path:'/tmp/firmware.img',bytes:1024})];
    assert.equal(last(context).isError,true);assert.match(last(context).content[0].text,/Recovered history is unverified/);return done;
  })});
  try {await agent.prompt('Continue',{recovery:fixture.recovery}); assert.equal(fixture.writes.length,0);}
  finally {fixture.device.setMode('auto');}
});
