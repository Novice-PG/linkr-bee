import { test, expect } from "@playwright/test";

/* Android 15+ forbids fitting the window above the system bars, and the
 * Android WebView reports env(safe-area-inset-*) as 0 even with
 * viewport-fit=cover (measured on Chromium 141). Capacitor's SystemBars plugin
 * injects the real WindowInsets as --safe-area-inset-* on <html>, so the layout
 * must prefer those variables and keep env() only as the iOS/browser fallback.
 *
 * The spec plays the plugin's part: it defines the variables on the root and
 * checks that the shell, the top bar and the settings drawer all move out of the
 * status and navigation bar bands. */

const INSETS = ":root{--safe-area-inset-top:44px;--safe-area-inset-bottom:36px;--safe-area-inset-left:0px;--safe-area-inset-right:0px}";

async function shellMetrics(page) {
  return page.evaluate(() => {
    const shell = document.querySelector(".app-shell");
    const topbar = document.querySelector(".topbar");
    const drawer = document.querySelector("#controlsPanel");
    const shellStyle = getComputedStyle(shell);
    const drawerStyle = getComputedStyle(drawer);
    return {
      shellPaddingTop: parseFloat(shellStyle.paddingTop),
      shellPaddingBottom: parseFloat(shellStyle.paddingBottom),
      topbarTop: topbar.getBoundingClientRect().top,
      drawerPaddingTop: parseFloat(drawerStyle.paddingTop),
      drawerPaddingBottom: parseFloat(drawerStyle.paddingBottom),
      viewportHeight: window.innerHeight,
    };
  });
}

test("the layout ignores the injected insets when they are absent (browser default)", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  const metrics = await shellMetrics(page);
  /* The gutter is the only padding on a plain browser page: env() is 0 there,
   * and no plugin has injected the custom properties. */
  expect(metrics.shellPaddingTop).toBeLessThan(44);
  expect(metrics.shellPaddingBottom).toBeLessThan(36);
  expect(metrics.topbarTop).toBe(metrics.shellPaddingTop);
});

test("injected status and navigation bar insets move the shell, top bar and drawer", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.addStyleTag({ content: INSETS });

  const metrics = await shellMetrics(page);
  expect(metrics.shellPaddingTop).toBe(44);
  expect(metrics.shellPaddingBottom).toBe(36);
  expect(metrics.topbarTop).toBe(44);

  /* The settings drawer is position: fixed on phones, so it does not inherit
   * the shell padding; it must carry the insets itself. */
  await page.locator("#panelToggle").tap();
  await expect(page.locator("#controlsPanel")).toBeVisible();
  const drawerMetrics = await page.evaluate(() => {
    const drawer = document.querySelector("#controlsPanel");
    const head = document.querySelector(".drawer-head");
    const style = getComputedStyle(drawer);
    const headTop = head.getBoundingClientRect().top;
    return {
      paddingTop: parseFloat(style.paddingTop),
      paddingBottom: parseFloat(style.paddingBottom),
      headTop,
      viewportHeight: window.innerHeight,
    };
  });
  expect(drawerMetrics.paddingTop).toBe(44);
  expect(drawerMetrics.paddingBottom).toBe(36);
  expect(drawerMetrics.headTop).toBeGreaterThanOrEqual(44);
});
