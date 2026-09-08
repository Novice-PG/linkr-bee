import { test, expect } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.route("**/app.js?*", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() +
      "\nwindow.__test = { state, setConnected, handleIncomingBytes, enqueueBytes, bleTransport };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__test?.state.term);
  await page.evaluate(() => {
    const { state, setConnected } = window.__test;
    window.sent = [];
    state.mode = "ws";
    state.ws = { readyState: 1, send: (bytes) => window.sent.push(Array.from(bytes)) };
    setConnected(true);
  });
  await page.locator("#agentButton").tap();
  await page.locator("#agentSettingsButton").tap();
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await page.locator("#agentApiKey").fill("test-device-key");
  await page.locator("#agentSettingsSave").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved");
  await page.locator("#drawerClose").tap();
});

function sse({ text, tool }) {
  const delta = tool ? { role: "assistant", tool_calls: [{ index: 0, id: "call_1", type: "function",
    function: { name: tool.name, arguments: JSON.stringify(tool.args) } }] }
    : { role: "assistant", content: text };
  const chunk = (delta, finish_reason) => JSON.stringify({ id: "chatcmpl-test", object: "chat.completion.chunk",
    created: 1, model: "test-model", choices: [{ index: 0, delta, finish_reason }] });
  return `data: ${chunk(delta, null)}\n\ndata: ${chunk({}, tool ? "tool_calls" : "stop")}\n\ndata: [DONE]\n\n`;
}

async function mockModel(page, respond) {
  let calls = 0;
  await page.route("https://agent.test/v1/chat/completions", async (route) => {
    const headers = { "access-control-allow-origin": "*", "access-control-allow-headers": "*" };
    if (route.request().method() === "OPTIONS") {
      await route.fulfill({ status: 204, headers });
      return;
    }
    const response = await respond(route.request().postDataJSON(), ++calls);
    await route.fulfill({ status: 200, headers, contentType: "text/event-stream", body: sse(response) });
  });
}
async function chooseMode(page, mode) {
  await page.locator("#agentModeButton").tap();
  await page.locator(`[name="agentMode"][value="${mode}"]`).locator('..').tap();
  await expect(page.locator("#agentModePicker")).toBeHidden();
}
async function ask(page, question = "Why did the target fail to boot?") {
  await page.locator("#agentQuestion").fill(question);
  await page.locator("#agentAsk").tap();
}

test("Pi reads real UART logs without exposing the saved key to model context", async ({ page }) => {
  let toolEvidence = "";
  await page.evaluate(() => {
    window.__test.handleIncomingBytes(new TextEncoder().encode("Kernel panic: unable to mount root\r\n"));
    window.__test.state.term.write("LOCAL_UI_ONLY");
  });
  await mockModel(page, (request, count) => {
    expect(JSON.stringify(request)).not.toContain("test-device-key");
    if (count === 1) return { tool: { name: "read_serial_log", args: {} } };
    toolEvidence = request.messages.find((message) => message.role === "tool")?.content;
    return { text: "The serial log reports a root filesystem mount failure." };
  });
  await ask(page);
  await expect(page.locator("#agentMessages")).toContainText("root filesystem mount failure");
  expect(toolEvidence).toContain("Kernel panic");
  expect(toolEvidence).not.toContain("LOCAL_UI_ONLY");
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  expect(toolEvidence).not.toContain("test-device-key");
  expect(await page.locator("#agentMessages").textContent()).not.toContain("test-device-key");
  await page.screenshot({ path: "test-results/agent-phone.png" });
});

test("unsaved configuration is not used, and running Agent settings are locked", async ({ page }) => {
  await page.locator("#agentSettingsButton").tap();
  await page.locator("#agentModel").fill("unsaved-model");
  await page.locator("#drawerClose").tap();
  await mockModel(page, (request, count) => {
    expect(request.model).toBe("test-model");
    return count === 1 ? { tool: { name: "send_serial_input", args: { text: "reboot", appendEnter: true } } }
      : { text: "Rejected input was not sent." };
  });
  await ask(page);
  await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toBeVisible();
  await page.locator("#agentSettingsButton").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText("Agent is running");
  for (const id of ["agentModel", "agentApiKey", "agentSettingsSave", "agentSettingsClear"]) {
    await expect(page.locator(`#${id}`)).toBeDisabled();
  }
  await page.locator("#drawerClose").tap();
  await page.locator("#agentMessages .agent-actions .btn:not(.btn-primary)").tap();
  await expect(page.locator("#agentMessages")).toContainText("Rejected input was not sent.");
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  await page.locator("#agentSettingsButton").tap();
  await expect(page.locator("#agentSettingsSave")).toBeEnabled();
  await expect(page.locator("#agentSettingsStatus")).toContainText("not been saved");
});

for (const changed of [false, true]) {
  test(`saved settings ${changed ? "start fresh when the model changes" : "retain the conversation when unchanged"}`, async ({ page }) => {
    let continued;
    await mockModel(page, (request, count) => {
      if (count === 1) return { text: "The root partition could not be mounted." };
      continued = request;
      return { text: "The next question uses the explicitly saved configuration." };
    });
    await ask(page, "Remember the board boots from eMMC");
    await expect(page.locator("#agentAsk")).toBeEnabled();
    await page.locator("#agentSettingsButton").tap();
    if (changed) await page.locator("#agentModel").fill("replacement-model");
    await page.locator("#agentSettingsSave").tap();
    await expect(page.locator("#agentSettingsStatus")).toContainText("saved on this device");
    await page.locator("#drawerClose").tap();
    await ask(page, "Continue diagnosing");
    await expect(page.locator("#agentMessages")).toContainText("explicitly saved configuration");
    expect(continued.model).toBe(changed ? "replacement-model" : "test-model");
    const context = JSON.stringify(continued.messages);
    if (changed) expect(context).not.toContain("boots from eMMC");
    else {
      expect(context).toContain("boots from eMMC");
      expect(context).toContain("could not be mounted");
    }
  });
}

test("serial input waits for explicit approval and returns to the Pi loop", async ({ page }) => {
  await chooseMode(page, "manual");
  await mockModel(page, (_request, count) => count === 1
    ? { tool: { name: "send_serial_input", args: { text: "uname -a", appendEnter: true } } }
    : { text: "Input sent. Read the target output to verify execution." });
  await ask(page, "Inspect the operating system");
  const approve = page.locator("#agentMessages .agent-actions .btn-primary");
  await expect(approve).toBeVisible();
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  await approve.tap();
  await expect.poll(() => page.evaluate(() => window.sent.length)).toBe(1);
  const sent = await page.evaluate(() => window.sent[0]);
  expect(new TextDecoder().decode(new Uint8Array(sent))).toMatch(/^uname -a\r?\n?$|^uname -a\r$/);
  await expect(page.locator("#agentMessages")).toContainText("Read the target output");
});

for (const [mode, command] of [["auto", "uname -a"], ["full-auto", "reboot"]]) {
  test(`${mode} sends permitted input directly and records the command`, async ({ page }) => {
    if (mode === "full-auto") await chooseMode(page, "full-auto");
    else await expect(page.locator('[name="agentMode"][value="auto"]')).toBeChecked();
    if (mode === "auto") await page.evaluate(() => window.__test.handleIncomingBytes(new TextEncoder().encode("root@board:~# ")));
    let systemPrompt = "";
    await mockModel(page, (request, count) => {
      systemPrompt = request.messages.find((message) => message.role === "system")?.content || "";
      return count === 1 ? { tool: { name: "send_serial_input", args: { text: command, appendEnter: true } } }
        : { text: "Command delivery recorded." };
    });
    await ask(page);
    await expect(page.locator("#agentMessages")).toContainText("Command delivery recorded.");
    expect(await page.evaluate(() => window.sent.length)).toBe(1);
    await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toHaveCount(0);
    await expect(page.locator("#agentMessages")).toContainText(command);
    expect(systemPrompt).toContain(mode === "auto" ? "Execution mode: Auto" : "Execution mode: Full Auto");
  });
}

test("switching to Full Auto cancels pending input instead of approving it", async ({ page }) => {
  await mockModel(page, () => ({ tool: { name: "send_serial_input", args: { text: "reboot", appendEnter: true } } }));
  await ask(page);
  const approve = page.locator("#agentMessages .agent-actions .btn-primary");
  await expect(approve).toBeVisible();
  await page.locator("#agentModeButton").tap();
  await page.keyboard.press("ArrowDown");
  await expect(page.locator('[name="agentMode"][value="auto"]')).toBeChecked();
  await page.keyboard.press("Escape");
  await expect(approve).toBeEnabled();
  await chooseMode(page, "full-auto");
  await expect(page.locator("#agentAsk")).toBeEnabled();
  await expect(approve).toBeDisabled();
  expect(await page.evaluate(() => window.sent)).toEqual([]);
});

test("mode changes keep completed diagnostic context and use the new policy on the next question", async ({ page }) => {
  let nextRequest;
  await mockModel(page, (request, count) => {
    if (count === 1) return { text: "The current hypothesis is a missing root filesystem." };
    nextRequest = request;
    return { text: "Continuing the root filesystem diagnosis in Manual mode." };
  });
  await ask(page, "Remember that this board boots from eMMC");
  await expect(page.locator("#agentMessages")).toContainText("missing root filesystem");
  await expect(page.locator("#agentAsk")).toBeEnabled();
  await chooseMode(page, "manual");
  await expect(page.locator("#agentMessages")).toContainText("conversation retained");
  await ask(page, "Continue the same diagnosis");
  await expect(page.locator("#agentMessages")).toContainText("Continuing the root filesystem diagnosis");
  expect(nextRequest.messages.find((message) => message.role === "system").content).toContain("Execution mode: Manual");
  expect(JSON.stringify(nextRequest.messages)).toContain("boots from eMMC");
  expect(JSON.stringify(nextRequest.messages)).toContain("missing root filesystem");
  expect(await page.evaluate(() => window.sent)).toEqual([]);
});

test("returning from another app preserves an idle diagnostic conversation", async ({ page }) => {
  let continued;
  await mockModel(page, (request, count) => {
    if (count === 1) return { text: "The suspected fault is the eMMC root filesystem." };
    continued = request;
    return { text: "Continuing the same diagnosis after returning to the app." };
  });
  await ask(page, "Remember this board boots from eMMC");
  await expect(page.locator("#agentAsk")).toBeEnabled();
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, value: true });
    document.dispatchEvent(new Event("visibilitychange"));
    delete document.hidden;
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await ask(page, "Continue the diagnosis");
  await expect(page.locator("#agentMessages")).toContainText("after returning to the app");
  expect(JSON.stringify(continued.messages)).toContain("boots from eMMC");
  expect(JSON.stringify(continued.messages)).toContain("suspected fault");
});

test("Escape during IME composition preserves the Agent and its question draft", async ({ page }) => {
  await page.locator("#agentQuestion").fill("分析启动失败");
  await page.locator("#agentQuestion").dispatchEvent("keydown", { key: "Escape", isComposing: true });
  await expect(page.locator("#agentPanel")).toBeVisible();
  await expect(page.locator("#agentQuestion")).toHaveValue("分析启动失败");
  await page.keyboard.press("Escape");
  await expect(page.locator("#agentPanel")).toBeHidden();
});

test("a mode change during approval preserves context but never sends the cancelled command", async ({ page }) => {
  let calls = 0, continued;
  await mockModel(page, (request) => {
    if (++calls === 1) return { tool: { name: "send_serial_input", args: { text: "reboot", appendEnter: true } } };
    continued = request;
    return { text: "The cancelled restart remains cancelled; continuing analysis only." };
  });
  await ask(page, "Check the eMMC boot failure; propose a restart");
  await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toBeVisible();
  await chooseMode(page, "full-auto");
  await expect(page.locator("#agentAsk")).toBeEnabled();
  await ask(page, "Continue analysis only");
  await expect(page.locator("#agentMessages")).toContainText("continuing analysis only");
  expect(JSON.stringify(continued.messages)).toContain("eMMC boot failure");
  expect(continued.messages.find((message) => message.role === "system").content).toContain("Execution mode: Full Auto");
  expect(await page.evaluate(() => window.sent)).toEqual([]);
});

test("long execution output exposes the final result in both tool context and the execution card", async ({ page }) => {
  await chooseMode(page, "full-auto");
  let evidence;
  await mockModel(page, async (request, count) => {
    if (count === 1) return { tool: { name: "send_serial_input", args: { text: "dmesg", appendEnter: true } } };
    const value = JSON.parse(request.messages.filter((message) => message.role === "tool").at(-1).content);
    if (count === 2) {
      await page.evaluate(() => window.__test.handleIncomingBytes(new TextEncoder().encode("x".repeat(6000) + "\r\nROOTFS_FAILURE_AT_END\r\nroot@board:~# ")));
      return { tool: { name: "inspect_serial_execution", args: { id: value.id } } };
    }
    evidence = value;
    return { text: "The final output reports a root filesystem failure." };
  });
  await ask(page);
  await expect(page.locator("#agentMessages")).toContainText("The final output reports");
  expect(evidence.evidence).toContain("ROOTFS_FAILURE_AT_END");
  expect(evidence.waitStatus).toBe("settled");
  expect(evidence.executionStatus).toBe("unknown");
  await expect(page.locator('[data-execution="serial-1"]')).toContainText("ROOTFS_FAILURE_AT_END");
});

test("gear picker navigates without changing mode until selection and dismisses without exiting Agent", async ({ page }) => {
  await expect(page.locator("#agentActiveMode")).toHaveText("Auto");
  await expect(page.locator("#agentModePicker")).toBeHidden();
  await page.locator("#agentModeButton").tap();
  await page.keyboard.press("ArrowUp");
  await expect(page.locator('[name="agentMode"][value="manual"]')).toBeFocused();
  await expect(page.locator('[name="agentMode"][value="auto"]')).toBeChecked();
  await page.keyboard.press("Enter");
  await expect(page.locator("#agentModePicker")).toBeHidden();
  await expect(page.locator("#agentActiveMode")).toHaveText("Manual");
  await expect(page.locator("#agentModeButton")).toBeFocused();
  await page.locator("#agentModeButton").tap();
  await page.keyboard.press("ArrowRight");
  await expect(page.locator('[name="agentMode"][value="auto"]')).toBeFocused();
  await expect(page.locator('[name="agentMode"][value="manual"]')).toBeChecked();
  await page.keyboard.press("Space");
  await expect(page.locator("#agentActiveMode")).toHaveText("Auto");
  await page.locator("#agentModeButton").tap();
  await page.keyboard.press("End");
  await page.keyboard.press("Escape");
  await expect(page.locator("#agentActiveMode")).toHaveText("Auto");
  await expect(page.locator("#agentPanel")).toBeVisible();
  await page.locator("#agentModeButton").tap();
  await page.locator("#agentModeClose").tap();
  await expect(page.locator("#agentModeButton")).toBeFocused();
  await expect(page.locator("#agentModePicker")).toBeHidden();
  await page.locator("#agentModeButton").tap();
  await page.locator("#agentQuestion").tap();
  await expect(page.locator("#agentModePicker")).toBeHidden();
  await expect(page.locator("#agentActiveMode")).toHaveText("Auto");
  for (const [mode, label] of [["full-auto", "Full Auto"], ["manual", "Manual"], ["auto", "Auto"]]) {
    await chooseMode(page, mode);
    await expect(page.locator("#agentActiveMode")).toHaveText(label);
  }
  expect(await page.evaluate(() => window.sent)).toEqual([]);
});

for (const [width, height] of [[320, 740], [375, 812], [802, 867], [1194, 834]]) {
  test(`gear selector fits the header and opens a vertical track at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    const heading = page.locator("#agentPanel .agent-header");
    await expect.poll(() => heading.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    const header = await heading.boundingBox();
    const button = await page.locator("#agentModeButton").boundingBox();
    expect(button.height).toBeGreaterThanOrEqual(44);
    expect(button.width).toBeGreaterThanOrEqual(44);
    expect(button.y + button.height).toBeLessThanOrEqual(header.y + header.height);
    await page.screenshot({ path: `test-results/agent-gears-${width}.png` });
    await page.locator("#agentModeButton").tap();
    await expect(page.locator("#agentModePicker")).toBeVisible();
    const rows = await page.locator("#agentModePicker label").all();
    const boxes = await Promise.all(rows.map((row) => row.boundingBox()));
    expect(boxes[0].y + boxes[0].height).toBeLessThanOrEqual(boxes[1].y);
    expect(boxes[1].y + boxes[1].height).toBeLessThanOrEqual(boxes[2].y);
    const picker = await page.locator("#agentModePicker").boundingBox();
    expect(picker.x).toBeGreaterThanOrEqual(0);
    expect(picker.x + picker.width).toBeLessThanOrEqual(width);
    expect(picker.y + picker.height).toBeLessThanOrEqual(height);
    await page.screenshot({ path: `test-results/agent-gears-open-${width}.png` });
    await page.locator('[name="agentMode"][value="manual"]').locator("..").tap();
    await expect(page.locator("#agentActiveMode")).toHaveText("Manual");
  });
}

test("gear selector stays usable above a landscape soft keyboard", async ({ page }) => {
  await page.setViewportSize({ width: 844, height: 390 });
  await page.locator("#agentQuestion").focus();
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 215 });
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("#terminalCard")).toBeHidden();
  for (const mode of ["full-auto", "manual", "auto"]) {
    await page.locator("#agentModeButton").tap();
    const picker = await page.locator("#agentModePicker").boundingBox();
    expect(picker.y).toBeGreaterThanOrEqual(0);
    expect(picker.y + picker.height).toBeLessThanOrEqual(215);
    await page.locator(`[name="agentMode"][value="${mode}"]`).locator("..").tap();
    await expect(page.locator("#agentModePicker")).toBeHidden();
    await expect(page.locator(`#agentModePicker [value="${mode}"]`)).toBeChecked();
  }
  expect(await page.evaluate(() => window.sent)).toEqual([]);
});

test("gear selector supports dark mode and reduced motion", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.locator("#agentSettingsButton").tap();
  await page.locator("#drawerThemeButton").tap();
  await page.locator("#drawerLangButton").tap();
  await page.locator("#drawerClose").tap();
  await page.locator("#agentModeButton").tap();
  await expect(page.locator("#agentModePicker")).toContainText("Auto · 推荐");
  expect(await page.locator(".agent-gear-track b").evaluate((el) => getComputedStyle(el).transitionDuration)).toBe("0s");
  await page.screenshot({ path: "test-results/agent-gears-dark.png" });
});

test("Auto asks before appending a query to a partially typed terminal line", async ({ page }) => {
  await page.evaluate(() => window.__test.enqueueBytes(new TextEncoder().encode("rm ")));
  await mockModel(page, () => ({ tool: { name: "send_serial_input", args: { text: "uname -a", appendEnter: true } } }));
  await ask(page);
  await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toBeVisible();
  expect(await page.evaluate(() => window.sent.length)).toBe(1);
  await page.locator("#agentStop").tap();
});

test("intervening terminal input invalidates an approved command", async ({ page }) => {
  await mockModel(page, (_request, count) => count === 1
    ? { tool: { name: "send_serial_input", args: { text: "reboot", appendEnter: true } } }
    : { text: "Input changed; request cancelled." });
  await ask(page);
  const approve = page.locator("#agentMessages .agent-actions .btn-primary");
  await expect(approve).toBeVisible();
  await page.evaluate(() => window.__test.enqueueBytes(new TextEncoder().encode("pwd\r")));
  await approve.tap();
  await expect(page.locator("#agentMessages")).toContainText("Terminal input changed");
  await expect(page.locator("#agentAsk")).toBeEnabled();
  expect(await page.evaluate(() => window.sent.length)).toBe(1);
});

for (const action of ["stop", "disconnect", "reject", "exit", "background"]) {
  test(`${action} prevents a pending agent write`, async ({ page }) => {
    await mockModel(page, (_request, count) => count === 1
      ? { tool: { name: "send_serial_input", args: { text: "reboot", appendEnter: true } } }
      : { text: "The request was not sent." });
    await ask(page, "Restart the target");
    await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toBeVisible();
    if (action === "stop") await page.locator("#agentStop").tap();
    if (action === "disconnect") await page.evaluate(() => window.__test.setConnected(false));
    if (action === "reject") await page.locator("#agentMessages .agent-actions .btn:not(.btn-primary)").tap();
    if (action === "exit") await page.locator("#agentClose").tap();
    if (action === "background") await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, value: true });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await expect(page.locator("#agentAsk")).toBeEnabled();
    expect(await page.evaluate(() => window.sent)).toEqual([]);
  });
}

test("new device sessions do not expose previous UART logs", async ({ page }) => {
  let evidence = "";
  await page.evaluate(() => {
    const { setConnected, handleIncomingBytes } = window.__test;
    handleIncomingBytes(new TextEncoder().encode("OLD_DEVICE_SECRET"));
    setConnected(false);
    setConnected(true);
    handleIncomingBytes(new TextEncoder().encode("NEW_DEVICE_BOOT"));
  });
  await mockModel(page, (request, count) => {
    if (count === 1) return { tool: { name: "read_serial_log", args: {} } };
    evidence = request.messages.find((message) => message.role === "tool")?.content;
    return { text: "New device log read." };
  });
  await ask(page);
  await expect(page.locator("#agentMessages")).toContainText("New device log read");
  expect(evidence).toContain("NEW_DEVICE_BOOT");
  expect(evidence).not.toContain("OLD_DEVICE_SECRET");
});

for (const [prompt, hint] of [["=> ", "Bootloader"], ["board login: ", "Login prompt"], ["Password: ", "Password prompt"]]) {
  test(`Auto waits for confirmation at ${hint}`, async ({ page }) => {
    await page.evaluate((prompt) => window.__test.handleIncomingBytes(new TextEncoder().encode(prompt)), prompt);
    await expect(page.locator("#agentConsole")).toContainText(hint);
    await mockModel(page, () => ({ tool: { name: "send_serial_input", args: { text: "uname -a", appendEnter: true } } }));
    await ask(page);
    await expect(page.locator("#agentMessages .agent-actions .btn-primary")).toBeVisible();
    expect(await page.evaluate(() => window.sent)).toEqual([]);
    await page.locator("#agentStop").tap();
  });
}

test("Pi inspects subsequent output without treating a returned prompt as command success", async ({ page }) => {
  await page.evaluate(() => window.__test.handleIncomingBytes(new TextEncoder().encode("root@board:~# ")));
  let record;
  await mockModel(page, async (request, count) => {
    if (count === 1) return { tool: { name: "send_serial_input", args: { text: "uname -a", appendEnter: true } } };
    const value = JSON.parse(request.messages.filter((message) => message.role === "tool").at(-1).content);
    if (count === 2) {
      expect(value.delivery).toBe("sent");
      expect(value.executionStatus).toBe("unknown");
      await page.evaluate(() => window.__test.handleIncomingBytes(new TextEncoder().encode("uname: not found\r\nroot@board:~# ")));
      return { tool: { name: "inspect_serial_execution", args: { id: value.id } } };
    }
    record = value;
    return { text: "The target reports uname is unavailable. A returned prompt does not mean success." };
  });
  await ask(page);
  await expect(page.locator("#agentMessages")).toContainText("uname is unavailable");
  expect(record.observation).toBe("prompt-returned");
  expect(record.executionStatus).toBe("unknown");
  expect(record.evidence).toContain("uname: not found");
  expect(await page.evaluate(() => window.sent.length)).toBe(1);
  await expect(page.locator('[data-execution="serial-1"]')).toContainText("uname: not found");
  await page.screenshot({ path: "test-results/agent-execution.png" });
});

test("AI keyboard collapses the terminal and restores it when dismissed", async ({ page }) => {
  await expect(page.locator("#terminalCard")).toBeVisible();
  const before = await page.locator("#terminalCard").boundingBox();
  const cols = await page.evaluate(() => window.__test.state.term.cols);
  await page.locator("#agentQuestion").focus();
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 420 });
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("#terminalCard")).toBeHidden();
  await page.evaluate(() => window.__test.handleIncomingBytes(new TextEncoder().encode("LOG_WHILE_COLLAPSED\r\n")));
  await expect(page.locator("#agentShowTerminal")).toBeVisible();
  expect(await page.evaluate(() => window.__test.state.term.cols)).toBe(cols);
  await expect.poll(() => page.locator("#agentAsk").evaluate((button) => button.getBoundingClientRect().bottom)).toBeLessThanOrEqual(420);
  await page.screenshot({ path: "test-results/agent-keyboard.png" });
  await page.evaluate(() => {
    delete visualViewport.height;
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("#terminalCard")).toBeVisible();
  await expect(page.locator("#agentShowTerminal")).toBeHidden();
  expect(await page.evaluate(() => Array.from({length: window.__test.state.term.buffer.active.length}, (_, i) =>
    window.__test.state.term.buffer.active.getLine(i)?.translateToString()).join("\n"))).toContain("LOG_WHILE_COLLAPSED");
  await expect.poll(async () => Math.abs((await page.locator("#terminalCard").boundingBox()).height - before.height)).toBeLessThan(2);
});

test("landscape keyboard keeps AI input and stop controls inside the visible viewport", async ({ page }) => {
  await page.setViewportSize({ width: 844, height: 390 });
  await page.locator("#agentQuestion").focus();
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 215 });
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("#terminalCard")).toBeHidden();
  await expect(page.locator("#agentActiveMode")).toBeVisible();
  await expect.poll(() => page.locator("#agentAsk").evaluate((button) => button.getBoundingClientRect().bottom)).toBeLessThanOrEqual(215);
  await page.screenshot({ path: "test-results/agent-keyboard-landscape.png" });
});

for (const [name, width, height, orientation] of [["phone", 390, 844, "rows"], ["tablet", 1194, 834, "columns"], ["narrow tablet window", 744, 1000, "rows"]]) {
  test(`${name} uses ${orientation} without replacing the terminal`, async ({ page }) => {
    await page.setViewportSize({ width, height });
    await expect(page.locator("#terminalWorkspace")).toHaveAttribute("data-agent-layout", orientation);
    const term = await page.locator("#terminalCard").boundingBox();
    const chat = await page.locator("#agentPanel").boundingBox();
    if (orientation === "rows") expect(term.y + term.height).toBeLessThanOrEqual(chat.y);
    else expect(term.x + term.width).toBeLessThanOrEqual(chat.x);
    expect(chat.x + chat.width).toBeLessThanOrEqual(width);
    expect(chat.y + chat.height).toBeLessThanOrEqual(height);
    await page.evaluate(() => { window.originalTerminal = window.__test.state.term;
      window.__test.handleIncomingBytes(new TextEncoder().encode("KEEP_THIS_LOG\r\n")); });
    await page.locator("#agentClose").tap();
    await expect(page.locator("#agentPanel")).toBeHidden();
    await page.locator("#agentButton").tap();
    await expect(page.locator("#agentPanel")).toBeVisible();
    expect(await page.evaluate(() => window.originalTerminal === window.__test.state.term)).toBe(true);
    expect(await page.evaluate(() => window.__test.state.term.buffer.active.getLine(0)?.translateToString() +
      Array.from({length: window.__test.state.term.buffer.active.length}, (_,i) => window.__test.state.term.buffer.active.getLine(i)?.translateToString()).join("\n"))).toContain("KEEP_THIS_LOG");
    await page.screenshot({ path: `test-results/agent-split-${width}.png` });
  });
}

test("show terminal switches input back to UART while the keyboard is open", async ({ page }) => {
  await page.locator("#agentQuestion").focus();
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 420 });
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("#terminalCard")).toBeHidden();
  await page.locator("#agentShowTerminal").tap();
  await expect(page.locator("#terminalCard")).toBeVisible();
  await page.locator('[data-terminal-key="Tab"]').tap();
  await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([9]);
  await page.locator("#agentQuestion").fill("This is an AI question");
  await expect(page.locator("#terminalCard")).toBeHidden();
  expect(await page.evaluate(() => window.sent.length)).toBe(1);
});

test("entering Agent mode exits native terminal fullscreen", async ({ page }) => {
  await page.locator("#agentClose").tap();
  await page.locator("#fullscreenBtn").tap();
  await page.waitForFunction(() => document.fullscreenElement);
  await page.locator("#agentButton").tap();
  await expect(page.locator("#agentPanel")).toBeVisible();
  await expect.poll(() => page.evaluate(() => document.fullscreenElement)).toBeNull();
  await expect(page.locator("#terminalCard")).toBeVisible();
});

test("cancelled BLE writes stop between fragments before a new device is used", async ({ page }) => {
  const result = await page.evaluate(async () => {
    const { state, setConnected, enqueueBytes, bleTransport } = window.__test;
    state.mode = "ble";
    state.nusReady = true;
    state.reliableReady = false;
    state.device = { id: "old-device" };
    document.querySelector("#chunkInput").value = "20";
    let writes = 0;
    bleTransport.write = async () => {
      writes++;
      setConnected(false);
      state.device = { id: "new-device" };
      setConnected(true);
    };
    try {
      await enqueueBytes(new Uint8Array(80), false, { signal: new AbortController().signal });
      return { writes, failed: false };
    } catch { return { writes, failed: true }; }
  });
  expect(result).toEqual({ writes: 1, failed: true });
});
