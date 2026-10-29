import { test, expect } from "@playwright/test";

const LINUX_UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";

async function linuxPage(browser, { bluetooth = false, demo = true } = {}) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    userAgent: LINUX_UA,
    isMobile: false,
    hasTouch: false,
  });
  await context.addInitScript((present) => {
    try {
      Object.defineProperty(navigator, "bluetooth", {
        configurable: true,
        value: present ? {} : undefined,
      });
    } catch {
      // Keep the browser's value when the property cannot be redefined.
    }
  }, bluetooth);
  const page = await context.newPage();
  await page.goto(demo ? "/?demo=board" : "/");
  await page.locator('.rail-btn[data-rail-target="connection"]').click();
  await page.waitForSelector("#bleModeBtn", { state: "visible" });
  return { context, page };
}

test("Linux Chromium without Web Bluetooth shows the flag and secure-context guide", async ({ browser }) => {
  const { context, page } = await linuxPage(browser);
  const trigger = page.locator(".ble-guidance");
  await expect(trigger).toBeVisible();
  await trigger.locator(".ble-guidance-open").click();
  const dialog = page.locator(".ble-guidance-dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("#enable-web-bluetooth");
  await expect(dialog).toContainText("localhost");
  await expect(dialog).toContainText("BlueZ");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await context.close();
});

test("the guide offers LAN mode and hides when LAN is selected", async ({ browser }) => {
  const { context, page } = await linuxPage(browser, { demo: false });
  await page.locator(".ble-guidance-open").click();
  await page.locator(".ble-guidance-dialog [data-lan]").click();
  await expect(page.locator(".ble-guidance")).toBeHidden();
  await expect(page.locator("#lanModeBtn")).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("#wsHostField")).toBeVisible();
  await context.close();
});

test("a browser with Web Bluetooth does not show the guide", async ({ browser }) => {
  const { context, page } = await linuxPage(browser, { bluetooth: true });
  await expect(page.locator(".ble-guidance")).toHaveCount(0);
  await context.close();
});
