import { test, expect } from '@playwright/test';

test.beforeEach(async ({ page }) => {
  await page.route('**/app.js?*', async route => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      '\nwindow.__latency = { state, setConnected, applySerialControl: typeof applySerialControl === "function" ? applySerialControl : null, onTerminalData, appendOutput, flushTerminalWrites };' });
  });
  await page.goto('/');
  await page.waitForFunction(() => window.__latency?.state.term);
  await page.evaluate(() => {
    const c = window.__latency;
    c.state.mode = 'ws'; c.state.ws = { readyState: 1, send() {} };
    c.setConnected(true);
    c.applySerialControl?.({ self: 1, owner: 1, mode: 'control', epoch: 2, peers: 1, capacity: 2 });
    c.flushTerminalWrites();
    document.querySelector('#localEchoInput').checked = false;
    window.parsedWrites = [];
    c.state.term.write = bytes => window.parsedWrites.push(new TextDecoder().decode(bytes));
    window.echoClock = 100;
    performance.now = () => window.echoClock;
    window.frames = new Map(); let frameId = 1000;
    requestAnimationFrame = callback => { window.frames.set(++frameId, callback); return frameId; };
    cancelAnimationFrame = id => window.frames.delete(id);
  });
});

test('a short remote echo reaches xterm without waiting for an outer frame', async ({ page }) => {
  const writes = await page.evaluate(() => {
    const c = window.__latency;
    c.onTerminalData('x');
    c.appendOutput('x');
    return window.parsedWrites;
  });
  expect(writes).toEqual(['x']);
});

test('interactive flush preserves older output and removes its scheduled frame', async ({ page }) => {
  const result = await page.evaluate(() => {
    const c = window.__latency;
    c.appendOutput('older\r\n');
    c.onTerminalData('x');
    c.appendOutput('x');
    return { writes: window.parsedWrites, frames: [...window.frames.values()].filter(callback => callback.name === 'flushTerminalWrites').length };
  });
  expect(result).toEqual({ writes: ['older\r\nx'], frames: 0 });
});

test('burst output stays batched and shortcut expires after typing stops', async ({ page }) => {
  const result = await page.evaluate(() => {
    const c = window.__latency;
    c.onTerminalData('x'); c.appendOutput('x');
    window.echoClock = 101;
    c.appendOutput('a'); c.appendOutput('b');
    const beforeFlush = [...window.parsedWrites];
    c.flushTerminalWrites();
    window.echoClock = 1101;
    c.appendOutput('idle');
    const afterExpiry = [...window.parsedWrites];
    c.flushTerminalWrites();
    return { beforeFlush, afterExpiry, final: window.parsedWrites };
  });
  expect(result).toEqual({ beforeFlush: ['x'], afterExpiry: ['x', 'ab'], final: ['x', 'ab', 'idle'] });
});

test('large output keeps batching even within the interactive window', async ({ page }) => {
  const result = await page.evaluate(() => {
    const c = window.__latency; c.onTerminalData('x');
    c.appendOutput('a'.repeat(8192)); c.appendOutput('x');
    const beforeFlush = window.parsedWrites.length;
    c.flushTerminalWrites();
    return { beforeFlush, output: window.parsedWrites.join('') };
  });
  expect(result.beforeFlush).toBe(0);
  expect(result.output).toBe('a'.repeat(8192) + 'x');
});
