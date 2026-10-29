import { test } from 'node:test';
import assert from 'node:assert/strict';
import { normalizeUartMode, uartTransportPlan } from '../../web/uart_transport_mode.js';

test('unknown or missing UART preference defaults to interaction first',()=>{
  for(const value of [null,undefined,'','bad','nus','interactive'])assert.equal(normalizeUartMode(value),'interactive');
  assert.equal(normalizeUartMode('reliable'),'reliable');
});
test('a capable board tries the selected channel before its compatible fallback',()=>{
  assert.deepEqual(uartTransportPlan('interactive',{reliable:true}),['nus','reliable']);
  assert.deepEqual(uartTransportPlan('reliable',{reliable:true}),['reliable','nus']);
});
test('legacy and NUS-only firmware never attempts a reliable subscription',()=>{
  for(const mode of ['interactive','reliable']){
    assert.deepEqual(uartTransportPlan(mode),['nus']);
    assert.deepEqual(uartTransportPlan(mode,{reliable:true,preferNus:true}),['nus']);
  }
});
