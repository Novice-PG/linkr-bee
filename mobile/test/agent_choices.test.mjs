import test from 'node:test';
import assert from 'node:assert/strict';
import {createAssistantMessageEventStream, getCurrentTools, getCurrentSystemPrompt} from '@earendil-works/pi-ai';
import {createSerialAgent} from '../src/pi-agent.mjs';
const question = {question:'Where should the file be saved?', options:[{label:'Target',description:'Save on the board.'},{label:'This computer'}]};
function fixture(requestUserInput, args = question) {
  let round=0, received, entered;
  const started = new Promise(resolve=>{entered=resolve});
  const status={connected:true,sessionId:1};
  const agent=createSerialAgent({config:{endpoint:'https://fixture.invalid/v1',model:'fixture'},requestUserInput,
    device:{mode:'auto',getStatus:()=>status,readLog:()=>({text:'',cursor:0}),execute:()=>assert.fail('Question must not write to UART')},
    stream:(model,context)=>{
      const content=++round===1 ? [{type:'toolCall',id:'choice-1',name:'ask_user',arguments:args}] : [{type:'text',text:'Answer received.'}];
      if(round===1){received={names:getCurrentTools(context.messages).map(tool=>tool.name),prompt:getCurrentSystemPrompt(context.messages)};entered();}
      else received.result=context.messages.findLast(message=>message.role==='toolResult');
      const message={role:'assistant',api:model.api,provider:model.provider,model:model.id,timestamp:Date.now(),content,
        stopReason:round===1?'toolUse':'stop',usage:{input:0,output:0,totalTokens:0,cacheRead:0,cacheWrite:0,cost:{input:0,output:0,total:0,cacheRead:0,cacheWrite:0}}};
      const output=createAssistantMessageEventStream();output.push({type:'done',reason:message.stopReason,message});output.end(message);return output;
    }});
  return {agent,started,status,get:()=>received};
}
test('a host without interactive input does not advertise the choice tool',async()=>{
  const f=fixture(null);await f.agent.prompt('Choose destination');assert(!f.get().names.includes('ask_user'));assert.equal(f.get().result.isError,true);
});
test('Pi pauses until an actual answer and supplies it to the next model turn',async()=>{
  let answer, entered;const asking=new Promise(resolve=>{entered=resolve});
  const f=fixture(args=>{assert.equal(args.question,question.question);entered();return new Promise(resolve=>{answer=resolve})});
  const run=f.agent.prompt('Choose destination');await asking;
  assert(f.get().names.includes('ask_user'));assert.match(f.get().prompt,/wait for their answer/);assert.equal(f.get().result,undefined);
  answer({status:'answered',selected:[1],text:'Keep the original filename'});await run;
  const result=JSON.parse(f.get().result.content[0].text);
  assert.deepEqual(result,{status:'answered',selected:[{index:1,label:'This computer'}],text:'Keep the original filename',grantsPermission:false});
});
test('multiple and free-text-only answers are preserved',async()=>{
  for(const answer of [{status:'answered',selected:[0,1],text:''},{status:'answered',selected:[],text:'Another destination'}]){
    const f=fixture(async()=>answer,{...question,multiple:true});await f.agent.prompt('Choose');
    assert.equal(f.get().result.isError,false);assert.equal(JSON.parse(f.get().result.content[0].text).text,answer.text);
  }
});
test('skip has no implied answer or execution permission',async()=>{
  const f=fixture(async()=>({status:'cancelled'}));await f.agent.prompt('Choose');
  assert.deepEqual(JSON.parse(f.get().result.content[0].text),{status:'cancelled',grantsPermission:false});
});
test('invalid or forged answers are refused',async()=>{
  for(const answer of [{status:'answered',selected:[9]}, {status:'answered',selected:[0,1]}, {status:'answered',selected:[0,0]}, {status:'answered',selected:[],text:''}, {selected:[0]}]){
    const f=fixture(async()=>answer);await f.agent.prompt('Choose');assert.equal(f.get().result.isError,true,JSON.stringify(answer));
  }
});
test('aborting a waiting question cancels it without another model call',async()=>{
  let asking;const started=new Promise(resolve=>{asking=resolve});let cancelled=false;
  const f=fixture(({signal})=>new Promise((resolve,reject)=>{signal.addEventListener('abort',()=>{cancelled=true;reject(signal.reason)},{once:true});asking();}));
  const run=f.agent.prompt('Choose');await started;f.agent.abort();await assert.rejects(run,/aborted/i);
  assert.equal(cancelled,true);assert.equal(f.get().result,undefined);
});
test('a late answer from a replaced device session is rejected',async()=>{
  const f=fixture(async()=>{f.status.sessionId=2;return{status:'answered',selected:[0]}});await f.agent.prompt('Choose').catch(()=>{});
  assert(!f.get().result || f.get().result.isError);
});
