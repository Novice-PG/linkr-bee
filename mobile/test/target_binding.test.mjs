import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,readFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {bindingReply,targetIdentityCommand,observedTargetId} from '../../web/target_binding.js';
const uuid='12345678-1234-4123-8123-123456789abc';
test('binding responses validate exact UUID or explicit removal',()=>{
 assert.equal(bindingReply('OK target='+uuid),uuid);assert.equal(bindingReply('OK target=none'),null);
 for(const text of ['ERR unsupported','OK','OK target=../../foo']) assert.throws(()=>bindingReply(text));
});
test('identity command creates once, preserves existing ID, and only explicitly regenerates',()=>{
 const root=mkdtempSync(join(tmpdir(),'linkr-id-'));
 const run=(action,id)=>execFileSync('sh',['-c',targetIdentityCommand(action,id,'LINKR_ID_abcd').replaceAll('/var/lib/linkr',root)],{encoding:'utf8'});
 try {
  run('bind',uuid);run('bind','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa');
  assert.equal(readFileSync(join(root,'device-id'),'utf8').trim(),uuid);
  assert.match(run('verify',uuid),new RegExp(uuid));
  run('regenerate','aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa');
  assert.equal(readFileSync(join(root,'device-id'),'utf8').trim(),'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa');
 }finally{rmSync(root,{recursive:true});}
});
test('failed, truncated or forged identity output never authorizes Flash storage',()=>{
 const record={executionStatus:'completed',exitCode:0,evidence:'\nLINKR_ID_abcd:'+uuid+'\n'};
 assert.equal(observedTargetId(record,'LINKR_ID_abcd'),uuid);
 for(const extra of [{exitCode:1},{evidenceTruncated:true},{evidence:'command echo LINKR_ID_abcd:'+uuid+'\n'}]) assert.throws(()=>observedTargetId({...record,...extra},'LINKR_ID_abcd'));
});

test('permission failure gives actionable status without accepting a UUID',()=>{
 assert.throws(()=>observedTargetId({executionStatus:'completed',exitCode:1,evidence:'mkdir: Permission denied\n'},'LINKR_ID_abcd'),error=>error.code==='TARGET_PERMISSION_DENIED');
});

import { mkdirSync, writeFileSync, existsSync, symlinkSync } from 'node:fs';
import { observedTargetPath } from '../../web/target_binding.js';
test('system first via sudo, fallback on denial, and existing user IDs stay stable',()=>{
 const root=mkdtempSync(join(tmpdir(),'linkr-id-policy-'));
 const bin=join(root,'bin'), home=join(root,'home'), system=join(root,'system','linkr');
 mkdirSync(bin);mkdirSync(home);
 writeFileSync(join(bin,'id'),'#!/bin/sh\nprintf "1000\\n"\n',{mode:0o755});
 const sudo=(success)=>writeFileSync(join(bin,'sudo'),`#!/bin/sh\necho invoked >> '${root}/sudo-called'\n${success ? 'shift 2; exec "$@"' : 'exit 1'}\n`,{mode:0o755});
 const run=()=>execFileSync('sh',['-c',targetIdentityCommand('bind',uuid,'LINKR_ID_abcd').replaceAll('/var/lib/linkr',system)],{encoding:'utf8',env:{...process.env,HOME:home,PATH:bin+':/usr/bin:/bin'}});
 try {
  sudo(true);
  const evidence=run();
  assert.equal(readFileSync(join(system,'device-id'),'utf8').trim(),uuid);
  assert.equal(observedTargetPath({evidence},'LINKR_ID_abcd'),join(system,'device-id'));
  assert.ok(existsSync(join(root,'sudo-called')));
  rmSync(join(root,'system'),{recursive:true});rmSync(join(root,'sudo-called'));
  sudo(false);run();
  const userId=join(home,'.local/share/linkr/device-id');
  assert.equal(readFileSync(userId,'utf8').trim(),uuid);
  rmSync(join(root,'sudo-called'));sudo(true);run();
  assert.equal(existsSync(join(root,'sudo-called')),false);
  assert.equal(existsSync(join(system,'device-id')),false);
  // A suspicious system identity must fail rather than shadow it in HOME.
  mkdirSync(system,{recursive:true});symlinkSync(userId,join(system,'device-id'));
  assert.throws(run);
 } finally {rmSync(root,{recursive:true,force:true});}
});
test('tracked binding commands fit the UART command budget',()=>{
 for(const action of ['verify','bind','regenerate']) {
  const command=targetIdentityCommand(action,uuid,'LINKR_ID_'+'a'.repeat(32));
  assert.ok(command.replaceAll("'","'\\''").length+120<=2048);
 }
});
