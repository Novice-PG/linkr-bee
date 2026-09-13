import { test, expect } from "@playwright/test";

const config = { endpoint: "https://agent.test/v1", model: "saved-model", apiKey: "saved-device-key" };
async function openSettings(page) {
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
  await expect(page.locator("#controlsPanel #agentSettings")).toBeVisible();
}
async function fillSettings(page, value = config) {
  await page.locator("#agentEndpoint").fill(value.endpoint);
  await page.locator("#agentModel").fill(value.model);
  await page.locator("#agentApiKey").fill(value.apiKey);
}
async function saveSettings(page) {
  await page.locator("#agentSettingsSave").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText("saved on this device");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
});

test("main settings saves the model and masked key without a chat, restores after reload, and clears", async ({ page }) => {
  let requests = 0;
  await page.route("https://agent.test/**", async (route) => { requests++; await route.abort(); });
  await openSettings(page);
  await expect(page.locator('[data-settings-page="connection"]').first()).toBeHidden();
  await expect(page.locator("#agentPanel")).toBeHidden();
  await fillSettings(page);
  await saveSettings(page);
  expect(requests).toBe(0);
  await page.reload();
  await page.locator("#panelToggle").tap();
  await expect(page.locator('[data-settings-target="ai"]')).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#agentEndpoint")).toHaveValue(config.endpoint);
  await expect(page.locator("#agentModel")).toHaveValue(config.model);
  await expect(page.locator("#agentApiKey")).toHaveValue(config.apiKey);
  await expect(page.locator("#agentApiKey")).toHaveAttribute("type", "password");
  await page.locator("#agentSettingsClear").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText("cleared");
  await page.reload();
  await openSettings(page);
  for (const id of ["agentEndpoint", "agentModel", "agentApiKey"]) await expect(page.locator(`#${id}`)).toHaveValue("");
  expect(await page.evaluate(() => localStorage.getItem("linkr-agent-model"))).toBeNull();
  expect(requests).toBe(0);
});

test("invalid edits and denied storage keep the saved settings and report the failure", async ({ page }) => {
  await openSettings(page);
  await fillSettings(page);
  await saveSettings(page);
  await page.locator("#agentEndpoint").fill("https://user:secret@agent.test/v1");
  await page.locator("#agentSettingsSave").tap();
  await expect(page.locator("#agentEndpoint")).toHaveAttribute("aria-invalid", "true");
  await expect(page.locator("#agentSettingsStatus")).toContainText("without credentials");
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("linkr-agent-model")))).toEqual({ ...config, headers: {}, provider: "openai-completions", reasoning: "off", contextWindow: 0, maxTokens: 0 });
  await fillSettings(page, { ...config, model: "unsaved-model" });
  await page.evaluate(() => {
    Storage.prototype.setItem = () => { throw new DOMException("Storage blocked", "QuotaExceededError"); };
    Storage.prototype.removeItem = () => { throw new DOMException("Storage blocked", "SecurityError"); };
  });
  await page.locator("#agentSettingsSave").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText("Save failed");
  await page.locator("#agentSettingsClear").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText("Clear failed");
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("linkr-agent-model")))).toEqual({ ...config, headers: {}, provider: "openai-completions", reasoning: "off", contextWindow: 0, maxTokens: 0 });
});

test("Agent settings uses the same drawer and returns to the existing conversation draft", async ({ page }) => {
  await page.locator("#agentButton").tap();
  await page.locator("#agentQuestion").fill("Keep my diagnostic question");
  // A missing configuration redirects Ask to the shared settings panel.
  await page.locator("#agentAsk").tap();
  await expect(page.locator("#controlsPanel #agentSettings")).toBeVisible();
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-modal", "true");
  expect(await page.locator(".workpanel").evaluate((el) => el.inert)).toBe(true);
  await fillSettings(page);
  await saveSettings(page);
  await page.locator("#drawerClose").tap();
  await expect(page.locator("#agentQuestion")).toHaveValue("Keep my diagnostic question");
  await expect(page.locator("#agentSettingsButton")).toBeFocused();
  expect(await page.locator(".workpanel").evaluate((el) => el.inert)).toBe(false);
  await page.locator("#agentSettingsButton").tap();
  await expect(page.locator("#agentApiKey")).toHaveValue(config.apiKey);
  await page.keyboard.press("Escape");
  await expect(page.locator("#agentPanel")).toBeVisible();
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "true");
});

test("Escape during IME composition keeps the settings drawer and unsaved input open", async ({ page }) => {
  await openSettings(page);
  await page.locator("#agentModel").fill("正在输入模型名");
  await page.locator("#agentModel").dispatchEvent("keydown", { key: "Escape", isComposing: true });
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "false");
  await expect(page.locator("#agentModel")).toHaveValue("正在输入模型名");
  await page.keyboard.press("Escape");
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "true");
});

for (const inAgent of [false, true]) {
  test(`settings remains editable above the phone keyboard (Agent: ${inAgent})`, async ({ page }) => {
    if (inAgent) {
      await page.locator("#agentButton").tap();
      await page.locator("#agentSettingsButton").tap();
    } else await openSettings(page);
    await fillSettings(page);
    await page.evaluate(() => {
      Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 360 });
      Object.defineProperty(visualViewport, "offsetTop", { configurable: true, get: () => 50 });
      visualViewport.dispatchEvent(new Event("resize"));
    });
    await expect(page.locator("#controlsPanel")).toBeVisible();
    await expect.poll(() => page.locator("#controlsPanel").evaluate((el) => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(410);
    await saveSettings(page);
    const save = await page.locator("#agentSettingsSave").boundingBox();
    expect(save.y).toBeGreaterThanOrEqual(50);
    expect(save.y + save.height).toBeLessThanOrEqual(410);
    await page.screenshot({ path: `test-results/agent-settings-keyboard-${inAgent}.png` });
    await page.locator("#drawerClose").tap();
    await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "true");
  });
}

test("tablet drawer exposes a translated AI tab alongside connection, serial and network", async ({ page }) => {
  await page.setViewportSize({ width: 802, height: 867 });
  await openSettings(page);
  await page.locator("#drawerLangButton").tap();
  await expect(page.locator("#agentSettingsTitle")).toHaveText("AI 配置");
  await expect(page.locator("#agentSettingsSave")).toHaveText("保存配置");
  await expect(page.locator(".settings-tab:visible")).toHaveCount(4);
  const drawer = await page.locator("#controlsPanel").boundingBox();
  expect(drawer.width).toBeLessThan(802);
  expect(drawer.x + drawer.width).toBeLessThanOrEqual(802);
  expect(await page.locator("#controlsPanel").evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.screenshot({ path: "test-results/agent-settings-tablet.png" });
});

test("desktop Agent can open shared settings after the normal sidebar was collapsed", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, hasTouch: false, isMobile: false });
  const page = await context.newPage();
  await page.goto("/");
  await page.locator("#panelToggle").click();
  await page.locator("#panelToggle").click();
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "true");
  await page.locator("#agentButton").click();
  await page.locator("#agentSettingsButton").click();
  await expect(page.locator("#controlsPanel #agentSettings")).toBeVisible();
  await expect(page.locator("#controlsPanel")).not.toHaveAttribute("aria-modal", "true");
  await expect(page.locator(".topbar")).toBeVisible();
  expect(await page.locator(".workpanel").evaluate((el) => el.inert)).toBe(false);
  await page.locator("#drawerClose").click();
  await expect(page.locator("#agentSettingsButton")).toBeFocused();
  await page.locator("#agentClose").click();
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "true");
  await page.locator("#panelToggle").click();
  await expect(page.locator("#agentSettings")).toBeVisible();
  await page.locator("#drawerClose").click();
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "true");
  await expect(page.locator("#panelToggle")).toBeFocused();
  // Narrow mouse-operated windows use the same right-hand sheet as tablets.
  await page.setViewportSize({ width: 802, height: 867 });
  await page.locator("#panelToggle").click();
  await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-modal", "true");
  await expect.poll(() => page.locator("#controlsPanel").evaluate((el) => el.getBoundingClientRect().right)).toBeLessThanOrEqual(802);
  const sheet = await page.locator("#controlsPanel").boundingBox();
  expect(sheet.x).toBeGreaterThan(0);
  expect(sheet.x + sheet.width).toBeLessThanOrEqual(802);
  await context.close();
});

for (const expanded of [false, true]) {
  test(`desktop Agent preserves the existing shell and sidebar (expanded: ${expanded})`, async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width: 1285, height: 867 }, hasTouch: false, isMobile: false });
    const page = await context.newPage();
    await page.goto("/");
    if (expanded) await page.locator("#panelToggle").click();
    // Wait for the sidebar transition before comparing the two workspaces.
    await expect.poll(() => page.locator("#controlsPanel").evaluate((el) => Math.round(el.getBoundingClientRect().width)))
      .toBe(expanded ? 300 : 2);
    const terminal = page.locator("#terminalCard");
    const before = await terminal.boundingBox();
    const header = await page.locator(".topbar").boundingBox();
    const sidebar = await page.locator("#controlsPanel").boundingBox();
    const saved = await page.evaluate(() => localStorage.getItem("linkr-sidebar"));
    await page.locator("#agentButton").click();
    await expect(page.locator("#agentPanel")).toBeVisible();
    await expect(page.locator(".topbar")).toBeVisible();
    await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", String(!expanded));
    await expect(page.locator("#controlsPanel")).not.toHaveAttribute("aria-modal", "true");
    expect(await page.locator(".topbar").boundingBox()).toEqual(header);
    expect(await page.locator("#controlsPanel").boundingBox()).toEqual(sidebar);
    const after = await terminal.boundingBox();
    expect(after.x).toBe(before.x);
    expect(after.y).toBe(before.y);
    expect(await page.evaluate(() => localStorage.getItem("linkr-sidebar"))).toBe(saved);
    await page.locator("#agentQuestion").fill("Keep this draft while changing the layout");
    // Both normal and Agent settings use the same docked panel, without blocking chat.
    await page.locator("#agentSettingsButton").click();
    await expect(page.locator("#agentSettings")).toBeVisible();
    await expect.poll(() => page.evaluate(() =>
      document.querySelector("#controlsPanel").getBoundingClientRect().right -
      document.querySelector("#terminalCard").getBoundingClientRect().left))
      .toBeLessThanOrEqual(0);
    expect(await page.locator(".workpanel").evaluate((el) => el.inert)).toBe(false);
    await page.locator("#agentQuestion").fill("Chat stays editable beside settings");
    const ask = await page.locator("#agentAsk").boundingBox();
    expect(ask.y + ask.height).toBeLessThanOrEqual(867);
    if (expanded) await page.locator("#themeButton").click();
    await page.screenshot({ path: `test-results/agent-desktop-sidebar-${expanded}.png` });

    // Resize across the drawer breakpoint, then restore the saved desktop state.
    await page.setViewportSize({ width: 802, height: 867 });
    await page.locator("#agentSettingsButton").click();
    await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-modal", "true");
    await page.setViewportSize({ width: 1285, height: 867 });
    await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "false");
    await expect(page.locator("#controlsPanel")).not.toHaveAttribute("aria-modal", "true");
    expect(await page.locator(".workpanel").evaluate((el) => el.inert)).toBe(false);
    await expect(page.locator("#agentQuestion")).toHaveValue("Chat stays editable beside settings");
    await page.locator("#agentClose").click();
    await expect(page.locator("#controlsPanel")).toHaveAttribute("aria-hidden", "false");
    await expect(page.locator(".topbar")).toBeVisible();
    await context.close();
  });
}

test("extra request headers reject malformed input and reach the model request", async ({ page }) => {
  let sentHeaders = null;
  await page.route("https://agent.test/v1/chat/completions", async (route) => {
    sentHeaders = route.request().headers();
    const chunk = (delta, finish) => JSON.stringify({ id: "x", object: "chat.completion.chunk",
      created: 1, model: "saved-model", choices: [{ index: 0, delta, finish_reason: finish }] });
    await route.fulfill({ status: 200, headers: { "access-control-allow-origin": "*", "access-control-allow-headers": "*" },
      contentType: "text/event-stream",
      body: `data: ${chunk({ role: "assistant", content: "Headers received." }, null)}\n\ndata: ${chunk({}, "stop")}\n\ndata: [DONE]\n\n` });
  });
  await openSettings(page);
  await fillSettings(page);
  await page.locator("#agentHeaders").fill("Bad Name: value");
  await page.locator("#agentSettingsSave").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText(/Invalid headers|请求头格式无效/);
  await expect(page.locator("#agentHeaders")).toHaveAttribute("aria-invalid", "true");

  await page.locator("#agentHeaders").fill("anthropic-dangerous-direct-browser-access: true");
  await saveSettings(page);
  await page.reload();
  await page.locator("#panelToggle").tap();
  await expect(page.locator("#agentHeaders")).toHaveValue("anthropic-dangerous-direct-browser-access: true");
  // Close the drawer before reaching for the toolbar behind it.
  await page.locator("#drawerClose").tap();

  await page.locator("#agentButton").click();
  await page.locator("#agentQuestion").fill("Are you reachable?");
  await page.locator("#agentAsk").click();
  await expect(page.locator("#agentMessages")).toContainText("Headers received.");
  expect(sentHeaders["anthropic-dangerous-direct-browser-access"]).toBe("true");
});
