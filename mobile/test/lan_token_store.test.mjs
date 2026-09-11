import test from 'node:test';
import assert from 'node:assert/strict';
import {createLanTokenStore} from '../../web/lan_token_store.js';
const A='a'.repeat(32), B='b'.repeat(32), C='c'.repeat(32);
function storage() {const values=new Map();return {getItem:k=>values.get(k),setItem:(k,v)=>values.set(k,v)};}
test('LAN tokens follow device identity, rotation and host aliases across reload',()=>{
 const disk=storage(),store=createLanTokenStore(disk);
 assert.equal(store.capture('A',A,'10.0.0.1'),A);
 assert.equal(store.capture('B',B,'10.0.0.2'),B);
 assert.equal(store.selectHost('ws://10.0.0.1/ws'),A);
 assert.equal(store.capture('A',C,'10.0.0.1'),C);
 assert.equal(createLanTokenStore(disk).selectHost('10.0.0.1'),C);
 assert.equal(store.selectHost('10.0.0.3'),'');
 assert.equal(store.capture('A','','10.0.0.1'),'');
 assert.equal(createLanTokenStore(disk).selectHost('10.0.0.1'),'');
});
test('automatic refresh preserves active manual draft but never transfers it to another device',()=>{
 const disk=storage(),store=createLanTokenStore(disk);
 store.capture('A',A,'10.0.0.1');store.edit('draft');
 assert.equal(store.capture('A',C,'10.0.0.1'),'draft');
 assert.equal(createLanTokenStore(disk).selectHost('10.0.0.1'),C);
 assert.equal(store.capture('B',B,'10.0.0.2'),B);
 assert.equal(store.selectHost('10.0.0.1'),C);
 store.edit(A);store.save();
 assert.equal(createLanTokenStore(disk).selectHost('10.0.0.1'),A);
});
