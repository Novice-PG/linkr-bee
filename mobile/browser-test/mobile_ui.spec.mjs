import { test, expect } from "@playwright/test";

async function openTerminal(page) {
  await page.route("**/app.js?*", async route => {
    const response = await route.fetch();
    await route.fulfill({ response, body: await response.text() + "\nwindow.__mobileUi = { state, setConnected };" });
  });
  await page.goto("/");
  await page.waitForFunction(() => window.__mobileUi?.state.term);
  await page.evaluate(() => {
    const { state, setConnected } = window.__mobileUi;
    window.sent = [];
    state.mode = "ws";
    state.ws = { readyState: 1, send: bytes => window.sent.push([...bytes]) };
    setConnected(true);
  });
}

test("favorite keys expand without writing UART or closing the keyboard", async ({ page }) => {
  await openTerminal(page);
  await expect(page.locator('[data-terminal-key="Tab"]')).toBeVisible();
  await expect(page.locator('[data-modifier="alt"]')).toBeHidden();
  await page.locator("#terminalKeysToggle").tap();
  await expect(page.locator('[data-modifier="alt"]')).toBeVisible();
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  await expect(page.locator(".xterm-helper-textarea")).toBeFocused();
  await page.locator('[data-modifier="alt"]').tap();
  await page.keyboard.type("c");
  await expect.poll(() => page.evaluate(() => window.sent.at(-1))).toEqual([27, 99]);
});

test("favorite choices survive reload and never send a command", async ({ page }) => {
  await openTerminal(page);
  await page.locator("#terminalKeysToggle").tap();
  await page.locator("#terminalKeysCustomize").tap();
  const dialog = page.locator("#terminalKeyPreferences");
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Control+Shift+K");
  await expect(page.locator("#agentPanel")).toBeHidden();
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  await dialog.locator('input[value="Tab"]').uncheck();
  await dialog.locator('input[value="modifier:alt"]').check();
  await dialog.locator("[data-done]").click();
  await page.locator("#terminalKeysToggle").tap();
  await expect(page.locator('[data-terminal-key="Tab"]')).toBeHidden();
  await expect(page.locator('[data-modifier="alt"]')).toBeVisible();
  expect(await page.evaluate(() => window.sent)).toEqual([]);
  await page.reload();
  await expect(page.locator('[data-terminal-key="Tab"]')).toBeHidden();
  await expect(page.locator('[data-modifier="alt"]')).toBeVisible();
  await page.locator("#terminalKeysToggle").tap();
  await page.locator("#terminalKeysCustomize").tap();
  await dialog.locator("[data-reset]").click();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await page.locator("#terminalKeysToggle").tap();
  await expect(page.locator('[data-terminal-key="Tab"]')).toBeVisible();
  await expect(page.locator('[data-modifier="alt"]')).toBeHidden();
});

test("denied preference storage keeps this session usable", async ({ page }) => {
  await openTerminal(page);
  await page.evaluate(() => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = function(key, value) {
      if (key === "linkr-terminal-favorite-keys") throw new Error("blocked");
      return original.call(this, key, value);
    };
  });
  await page.locator("#terminalKeysToggle").tap();
  await page.locator("#terminalKeysCustomize").tap();
  const dialog = page.locator("#terminalKeyPreferences");
  await dialog.locator('input[value="modifier:alt"]').check();
  await expect(dialog.locator('[role="status"]')).toContainText(/this session|本次会话/);
  await dialog.locator("[data-done]").click();
  await page.locator("#terminalKeysToggle").tap();
  await expect(page.locator('[data-modifier="alt"]')).toBeVisible();
});

test("compact landscape gives the output more space and collapses extra keys for IME", async ({ page }) => {
  await page.setViewportSize({ width: 844, height: 390 });
  await openTerminal(page);
  await expect(page.locator("#presetChips")).toBeHidden();
  await expect.poll(() => page.locator("#terminalOutput").evaluate(el => el.clientHeight)).toBeGreaterThan(150);
  await page.locator("#terminalKeysToggle").tap();
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 230 });
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("html")).toHaveClass(/soft-keyboard-open/);
  await expect(page.locator("#terminalKeysToggle")).toHaveAttribute("aria-expanded", "false");
  await expect.poll(() => page.locator("#terminalKeyBar").evaluate(el => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(230);
});

test("advanced settings reveal validation errors and preserve hidden configuration", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
  const advanced = page.locator("#agentAdvancedSettings");
  await expect(advanced).not.toHaveAttribute("open");
  await expect.poll(() => page.locator("#agentSettingsSave").evaluate(el => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(844);
  await page.locator("#agentEndpoint").fill("https://agent.test/v1");
  await page.locator("#agentModel").fill("test-model");
  await advanced.locator("summary").tap();
  await expect.poll(() => page.locator("#agentSettingsSave").evaluate(el => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(844);
  await page.locator("#agentHeaders").fill("Bad Name: value");
  await advanced.locator("summary").tap();
  await page.locator("#agentSettingsSave").tap();
  await expect(advanced).toHaveAttribute("open", "");
  await expect(page.locator("#agentHeaders")).toBeFocused();
  await page.locator("#agentHeaders").fill("x-test: preserved");
  await advanced.locator("summary").tap();
  await page.locator("#agentSettingsSave").tap();
  await expect(page.locator("#agentSettingsStatus")).toContainText(/saved|已保存/);
  await page.reload();
  await page.locator("#panelToggle").tap();
  await expect(page.locator("#agentHeaders")).toHaveValue("x-test: preserved");
  await expect(page.locator("#agentAdvancedSettings")).not.toHaveAttribute("open");
});

test("landscape settings keep an editable field and save controls above the IME", async ({ page }) => {
  await page.setViewportSize({ width: 844, height: 390 });
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
  await page.locator("#agentModel").fill("keyboard-model");
  await page.evaluate(() => {
    Object.defineProperty(visualViewport, "height", { configurable: true, get: () => 230 });
    visualViewport.dispatchEvent(new Event("resize"));
  });
  await expect(page.locator("html")).toHaveClass(/soft-keyboard-open/);
  await expect.poll(() => page.locator(".agent-settings-scroll").evaluate(el => el.clientHeight)).toBeGreaterThanOrEqual(44);
  await expect.poll(() => page.locator("#agentSettingsSave").evaluate(el => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(230);
  await expect(page.locator("#drawerClose")).toBeVisible();
});
