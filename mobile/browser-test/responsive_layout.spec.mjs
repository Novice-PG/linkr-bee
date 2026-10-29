import { test, expect } from "@playwright/test";

async function openBoard(page, width, height) {
  await page.setViewportSize({ width, height });
  await page.goto("/?demo=board");
  await page.waitForSelector("#terminalCard");
  await page.waitForTimeout(250);
}

async function metrics(page) {
  return page.evaluate(() => {
    const rect = (element) => {
      const box = element.getBoundingClientRect();
      return { left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: box.width, height: box.height };
    };
    const tabs = [...document.querySelectorAll(".settings-tab")].filter((tab) => !tab.hidden && getComputedStyle(tab).display !== "none" && tab.getClientRects().length > 0);
    const tabBox = document.querySelector(".settings-tabs");
    const keyScroll = document.querySelector(".terminal-key-scroll");
    const keys = keyScroll ? [...keyScroll.querySelectorAll("button")].filter((key) => getComputedStyle(key).display !== "none") : [];
    return {
      pageOverflow: document.documentElement.scrollWidth > window.innerWidth + 1,
      tabs: tabs.map((tab) => ({ ...rect(tab), label: tab.textContent.trim(), textOverflow: tab.scrollWidth > tab.clientWidth + 1 })),
      tabBox: tabBox && !tabBox.hidden ? { ...rect(tabBox), rows: new Set(tabs.map((tab) => Math.round(tab.getBoundingClientRect().top))).size } : null,
      deviceName: (() => {
        const element = document.querySelector(".device-name");
        return element ? { display: getComputedStyle(element).display, text: element.textContent.trim(), ...rect(element) } : null;
      })(),
      keyScroll: keyScroll ? { ...rect(keyScroll), clientWidth: keyScroll.clientWidth, scrollWidth: keyScroll.scrollWidth } : null,
      keys: keys.map((key) => ({ ...rect(key), label: key.textContent.trim() || key.getAttribute("aria-label") })),
    };
  });
}

for (const width of [320, 360, 390]) {
  test(`phone ${width} keeps every favourite key and all six tabs fully visible`, async ({ page }) => {
    await openBoard(page, width, 844);
    const result = await metrics(page);
    expect(result.pageOverflow).toBe(false);
    expect(result.keyScroll).not.toBeNull();
    expect(result.keys.length).toBeGreaterThanOrEqual(8);
    for (const key of result.keys) {
      expect(key.left).toBeGreaterThanOrEqual(result.keyScroll.left - 1);
      expect(key.right).toBeLessThanOrEqual(result.keyScroll.right + 1);
      expect(key.bottom).toBeLessThanOrEqual(result.keyScroll.bottom + 1);
    }
    expect(result.keyScroll.scrollWidth).toBeLessThanOrEqual(result.keyScroll.clientWidth + 1);
    expect(result.tabBox?.rows).toBe(1);
    for (const tab of result.tabs) {
      expect(tab.textOverflow, tab.label).toBe(false);
      expect(tab.width).toBeGreaterThanOrEqual(44);
    }
    expect(result.tabs.length).toBe(6);
    expect(result.tabs.at(-1).right).toBeLessThanOrEqual(result.tabBox.right + 1);
    expect(result.deviceName?.display).toBe("none");
    expect(await page.locator("#compactDevice").isVisible()).toBe(true);
    expect(await page.locator("#compactDevice").textContent()).toContain("Linkr Bee");
  });
}

for (const width of [601, 768, 834, 900]) {
  test(`tablet ${width} keeps labelled tabs and the device name without overflowing`, async ({ page }) => {
    await openBoard(page, width, 1000);
    const result = await metrics(page);
    expect(result.pageOverflow).toBe(false);
    expect(result.tabs.length).toBe(6);
    expect(result.tabBox?.rows).toBe(2);
    for (const tab of result.tabs) {
      expect(tab.textOverflow, tab.label).toBe(false);
      expect(tab.width).toBeGreaterThanOrEqual(44);
    }
    expect(result.tabs.find((tab) => tab.label === "AI").right).toBeLessThanOrEqual(result.tabBox.right + 1);
    expect(result.deviceName?.display).not.toBe("none");
    expect(result.deviceName?.text).toContain("Linkr Bee");
    expect(result.keyScroll?.scrollWidth).toBeLessThanOrEqual((result.keyScroll?.clientWidth ?? 0) + 1);
  });
}

test("desktop 1440 shows the rail and hides the duplicate tab titlebar", async ({ browser }) => {
  /* The default project context is a touch phone; a 1440px viewport there is
   * still a coarse-pointer drawer layout. Use a real pointer desktop context. */
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });
  const page = await context.newPage();
  await page.goto("/?demo=board");
  await page.waitForSelector("#terminalCard");
  await page.waitForTimeout(250);
  const result = await metrics(page);
  expect(result.pageOverflow).toBe(false);
  expect(await page.locator(".settings-tabs").isVisible()).toBe(false);
  expect(await page.locator(".rail-btn:visible").count()).toBeGreaterThan(0);
  expect(result.tabs.length).toBe(0);
  expect(result.deviceName?.display).not.toBe("none");
  await context.close();
});

test("desktop keeps the rail visible while the terminal AI assistant is open", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, isMobile: false, hasTouch: false });
  const page = await context.newPage();
  await page.goto("/?demo=board");
  await page.waitForSelector("#agentButton");
  await page.locator("#agentButton").click();
  await expect(page.locator("html")).toHaveClass(/agent-mode/);
  await expect(page.locator(".rail")).toBeVisible();
  await expect(page.locator("#agentPanel")).toBeVisible();
  await context.close();
});
