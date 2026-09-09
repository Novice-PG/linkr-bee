import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync,writeFileSync,readFileSync,existsSync,rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { targetDownloadPlan } from '../../web/download_plan.js';
import { fetchDownload } from '../../web/local_download.js';
const probe={executionStatus:'completed',exitCode:0,evidence:'LINKR_TOOL:curl\nLINKR_TOOL:sha256sum\n'};

test('target plans require completed probes and absolute destinations',()=>{
 for(const p of [null,{...probe,executionStatus:'unknown'},{...probe,evidenceTruncated:true},{...probe,evidence:''}]) assert.throws(()=>targetDownloadPlan({url:'https://example.org/a',path:'/tmp/a'},p));
 assert.throws(()=>targetDownloadPlan({url:'https://example.org/a',path:'relative'},probe));
 assert.throws(()=>targetDownloadPlan({url:'file:///a',path:'/tmp/a'},probe));
});

test('target downloads verify hashes and never overwrite destinations',()=>{
 const dir=mkdtempSync(join(tmpdir(),'linkr-download-test-'));
 try {
  // Offline tool fixture: no external network or target hardware.
  writeFileSync(join(dir,'curl'), '#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\nprintf payload > "$2"\n',{mode:0o755});
  writeFileSync(join(dir,'sha256sum'), '#!/bin/sh\nshasum -a 256 "$1"\n',{mode:0o755});
  const path=join(dir,"file's name.bin");
  const sha256=createHash('sha256').update('payload').digest('hex');
  const run=(args)=>spawnSync('sh',['-c',targetDownloadPlan({url:'https://example.org/a',path,...args},probe).command],{env:{...process.env,PATH:dir+':'+process.env.PATH},encoding:'utf8'});
  assert.equal(run({sha256:'0'.repeat(64)}).status,65);
  assert.equal(existsSync(path),false);
  const good=run({sha256});
  assert.equal(good.status,0,good.stderr);
  assert.match(good.stdout,new RegExp('LINKR_SHA256:'+sha256));
  assert.equal(readFileSync(path,'utf8'),'payload');
  assert.equal(run({sha256}).status,73);
 } finally { rmSync(dir,{recursive:true,force:true}); }
});

test('computer downloads report progress and checksum without UART',async()=>{
 const events=[];
 const sha256=createHash('sha256').update('payload').digest('hex');
 const fetchImpl=async(_url,options)=>{assert.equal(options.credentials,'omit');return new Response('payload',{headers:{'content-length':'7'}});};
 const result=await fetchDownload({url:'https://example.org/a',sha256,fetchImpl,onProgress:p=>events.push(p)});
 assert.equal(result.sha256,sha256);assert.equal(result.checksumStatus,'matched');assert.deepEqual(events,[{bytes:7,total:7}]);
 await assert.rejects(fetchDownload({url:'https://example.org/a',sha256:'0'.repeat(64),fetchImpl}),/mismatch/);
 await assert.rejects(fetchDownload({url:'https://example.org/a',fetchImpl,maxBytes:2}),/limit/);
});
