import test from 'node:test';
import assert from 'node:assert/strict';
import {createTaskStore, deviceIdentity, TASKS_KEY} from '../../web/agent_tasks.js';
import {parseDeviceProfile} from '../../web/device_profile.js';
import {evidenceExcerpt} from '../src/agent-context.mjs';

test('task summaries survive reload without restoring executable commands and stay device isolated', () => {
  const data = new Map(), storage = {getItem:k=>data.get(k),setItem:(k,v)=>data.set(k,v)};
  const key = deviceIdentity({transport:'ble',deviceId:'bee-a',uart:'115200'});
  createTaskStore(storage).save({id:'one',deviceKey:key,goal:'diagnose',status:'running',summary:'token=private https://user:pass@example.com/a?key=secret',executions:[{payload:'reboot',delivery:'unknown'}]});
  const [task] = createTaskStore(storage).list(key);
  assert.equal(task.status,'interrupted'); assert.equal(task.historical,true);
  assert.equal(createTaskStore(storage).list('bee-b').length,0);
  assert.doesNotMatch(data.get(TASKS_KEY), /private|user:pass|key=secret|reboot/);
  createTaskStore(storage).clear(key); assert.equal(createTaskStore(storage).list(key).length,0);
});
test('malformed storage is ignored and quota errors are surfaced', () => {
  const store=createTaskStore({getItem:()=>'{',setItem:()=>{throw new Error('quota');}});
  assert.deepEqual(store.list('a'),[]);
  assert.throws(()=>store.save({id:'x',deviceKey:'a',goal:'test'}),/quota/);
});
test('profile requires complete output markers and preserves tool observations', () => {
  const text='LINKR_PROFILE_BEGIN\r\nLinux board\r\nLINKR_OS\r\nID=debian\r\nLINKR_MODEL\r\nBoard\0\r\nLINKR_BOOT\r\nboot-1\r\nLINKR_DISK\r\n/dev/root 100 40 60\r\nLINKR_TOOLS\r\nTOOL:curl\r\nTOOL:sha256sum\r\nLINKR_PROFILE_END\r\n';
  assert.equal(parseDeviceProfile(text.slice(0,-2)),null);
  const profile=parseDeviceProfile(text); assert.equal(profile.model,'Board'); assert.deepEqual(profile.tools,['curl','sha256sum']);
});
test('context excerpts retain middle failures while remaining bounded',()=>{
  const input='start\n'+'normal\n'.repeat(500)+'fatal: no space left on device\n'+'normal\n'.repeat(500)+'end';
  const excerpt=evidenceExcerpt(input,1200);
  assert.match(excerpt,/fatal: no space/); assert.match(excerpt,/omitted/); assert.ok(excerpt.length<=1200);
});
