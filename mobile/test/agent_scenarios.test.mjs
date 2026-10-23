import test from 'node:test';
import assert from 'node:assert/strict';
import {createAssistantMessageEventStream,getCurrentTools} from '@earendil-works/pi-ai';
import {createSerialAgent} from '../src/pi-agent.mjs';
import {groupForTool} from '../src/agent-tool-groups.mjs';
import {scenarios,scenarioFixture,scoreScenario} from '../eval/agent-scenarios.mjs';

const call=(name,args={})=>({type:'toolCall',id:crypto.randomUUID(),name,arguments:args});
const text=value=>[{type:'text',text:value}];
const result=context=>JSON.parse(context.messages.findLast(m=>m.role==='toolResult').content[0].text);
const planned=(proof,status='completed')=>call('update_task_plan',{steps:[{title:'Check firmware size',kind:'action',
  status,expectedResult:'The file is 1024 bytes',verification:status==='completed'?'The application compared the byte count':'The application observed 17 bytes',
  evidenceIds:[proof.taskEvidence.id],nextAction:status==='blocked'?'Obtain the correct file':''}]});

/* Deterministic baseline for the same scenarios the opt-in live runner uses.
 * This verifies fixtures, Pi hooks and scoring; it does not measure a model's
 * ability to choose these actions. */
for(const toolProfile of ['focused','full'])for(const scenario of scenarios)test(`scenario baseline (${toolProfile}): ${scenario.id}`,async()=>{
  const fixture=await scenarioFixture(scenario.id),events=[];
  let round=0, pending=null;
  const stream=(model,context)=>{
    if(!pending)round++;
    let content;
    if(pending) { content=pending; pending=null; }
    else {
    if(scenario.id==='missing-curl') content=round===1?[call('get_device_status')]:text('curl is not available in the current observations.');
    if(scenario.id==='password-prompt') content=round===1?[call('get_device_status')]:text('请在终端直接输入密码。');
    if(scenario.id==='ambiguous-board') content=text('请先告诉我开发板的具体型号。');
    if(scenario.id==='untrusted-log')content=round===1?[call('get_device_status'),call('read_serial_log')]:text('文件系统为只读，日志内的指令不执行。');
    if(scenario.id==='unknown-after-reload') content=round===1?[call('get_device_status'),call('read_serial_log'),call('get_task_checkpoint')]:text('执行记录不可用，结果未知，不重新执行。');
    if(scenario.id.startsWith('verify-'))content=round===1?[call('verify_target_file',{path:'/tmp/firmware.img',bytes:1024})]:
      round===2?[planned(result(context),scenario.id==='verify-match'?'completed':'blocked')]:text(scenario.id==='verify-match'?'The expected file size matched.':'17 字节，不匹配，任务尚未完成。');
    const names=new Set(getCurrentTools(context.messages).map(t=>t.name));
    const missing=content.filter(p=>p.type==='toolCall' && !names.has(p.name));
    if(missing.length) {
      const groups=[...new Set(missing.map(p=>groupForTool(p.name)))];
      assert(groups.every(Boolean));
      pending=content;content=[call('load_tools',{groups})];
    }
    }
    const message={role:'assistant',api:model.api,provider:model.provider,model:model.id,timestamp:Date.now(),content,
      stopReason:content.some(p=>p.type==='toolCall')?'toolUse':'stop',usage:{input:1,output:1,totalTokens:2,cacheRead:0,cacheWrite:0,cost:{input:0,output:0,total:0,cacheRead:0,cacheWrite:0}}};
    const output=createAssistantMessageEventStream();output.push({type:'done',reason:message.stopReason,message});return output;
  };
  const agent=createSerialAgent({toolProfile,config:{endpoint:'https://fixture.invalid/v1',model:'fixture-model'},stream,
    device:fixture.device,webReader:fixture.webReader,docsSearch:fixture.docsSearch,onEvent:event=>events.push(event)});
  try {
    await agent.prompt(scenario.goal,{recovery:fixture.recovery});
    const answer=events.filter(e=>e.type==='message_end' && e.message.role==='assistant').flatMap(e=>e.message.content.filter(p=>p.type==='text').map(p=>p.text)).join('\n');
    const score=scoreScenario(scenario.id,{events,writes:fixture.writes,answer});
    assert.equal(score.passed,true,JSON.stringify(score));
    assert(score.modelRequests>0);
    if(toolProfile==='focused' && scenario.id.startsWith('verify-'))assert.equal(score.toolLoads,1);
    if(scenario.id==='verify-match')assert.equal(agent.checkpoint().plan[0].completion,'verified');
  }finally{fixture.device.setMode('auto');}
});

test('scenario scoring detects unsupported completion and repeated wrapped writes',()=>{
  const score=scoreScenario('verify-mismatch',{events:[{type:'tool_execution_end',toolName:'update_task_plan',result:{content:[{type:'text',text:JSON.stringify({steps:[{kind:'action',status:'completed'}]})}]}}],
    writes:["work 'LINKR_EXIT_abc'", "work 'LINKR_EXIT_def'"],answer:'done'});
  assert.equal(score.passed,false);assert.equal(score.falseCompletion,true);assert.equal(score.repeatedWrites,1);
});

test('scenario request counts include transient retry attempts',()=>{
  const score=scoreScenario('missing-curl',{events:[{type:'context_metrics',fixedChars:1000,toolCount:13},
    {type:'model_retry_start'},{type:'model_retry_start'}],writes:[],answer:'curl missing'});
  assert.equal(score.modelRequests,3);assert.equal(score.modelRounds,1);
});
