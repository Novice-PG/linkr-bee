/* Offline schema/prompt measurements, not a tokenizer or model-quality test.
 * No network, credentials or hardware are used. Include optional client
 * capabilities in both profiles for a like-for-like fixed-cost comparison. */
import {createAssistantMessageEventStream} from '@earendil-works/pi-ai';
import {createSerialAgent} from '../src/pi-agent.mjs';

const report=[];
for(const toolProfile of ['full','focused']) {
  const agent=createSerialAgent({toolProfile,config:{endpoint:'https://offline.invalid/v1',model:'offline'},
    device:{mode:'auto',getStatus:()=>({connected:false,sessionId:1})},accessory:{},notes:{},
    onEvent:event=>{if(event.type==='context_metrics')report.push({toolProfile,...event});},
    stream:(model)=>{
      const message={role:'assistant',api:model.api,provider:model.provider,model:model.id,timestamp:1,
        content:[{type:'text',text:'Offline fixture'}],stopReason:'stop',
        usage:{input:0,output:0,totalTokens:0,cacheRead:0,cacheWrite:0,cost:{input:0,output:0,total:0,cacheRead:0,cacheWrite:0}}};
      const output=createAssistantMessageEventStream();
      output.push({type:'done',reason:'stop',message});output.end(message);return output;
    }});
  await agent.prompt('Explain the boot log without changing the target.');
}
for(const entry of report)console.log(JSON.stringify(entry));
console.log(JSON.stringify({fixedContextReductionPercent:Math.round((1-report[1].fixedChars/report[0].fixedChars)*1000)/10,
  note:'Characters, not billed tokens. No real model requests or UART writes.'}));
