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
   * the shell padding. Its scrollport starts at the top of the screen: the
   * sticky header carries the status-bar inset as padding and covers that band,
   * and the sticky tabs sit directly below it. */
  await page.locator("#panelToggle").tap();
  await expect(page.locator("#controlsPanel")).toBeVisible();
  const drawerMetrics = await page.evaluate(() => {
    const drawer = document.querySelector("#controlsPanel");
    const head = document.querySelector(".drawer-head");
    const tabs = document.querySelector(".settings-tabs");
    const drawerStyle = getComputedStyle(drawer);
    const headStyle = getComputedStyle(head);
    const tabsRect = tabs.getBoundingClientRect();
    const headRect = head.getBoundingClientRect();
    return {
      paddingTop: parseFloat(drawerStyle.paddingTop),
      paddingBottom: parseFloat(drawerStyle.paddingBottom),
      headTop: headRect.top,
      headHeight: headRect.height,
      headPaddingTop: parseFloat(headStyle.paddingTop),
      headBackground: headStyle.backgroundColor,
      tabsTop: tabsRect.top,
    };
  });
  expect(drawerMetrics.paddingTop).toBe(0);
  expect(drawerMetrics.paddingBottom).toBe(36);
  expect(drawerMetrics.headTop).toBe(0);
  expect(drawerMetrics.headPaddingTop).toBeGreaterThanOrEqual(44);
  expect(drawerMetrics.headHeight).toBeGreaterThanOrEqual(100);
  /* At rest the tabs sit below the header with the flex gap; once the drawer
   * scrolls they stick right under it. */
  expect(drawerMetrics.tabsTop).toBeGreaterThan(drawerMetrics.headHeight);
  /* The sticky chrome must hide the cards scrolling behind it: a translucent
   * glass background would let their text show through as overlapping UI. */
  expect(drawerMetrics.headBackground).not.toContain("rgba");
});

test("a phone drawer keeps its sticky header over the status bar and the cards behind it", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.addStyleTag({ content: INSETS });
  await page.locator("#panelToggle").tap();
  await expect(page.locator("#controlsPanel")).toBeVisible();
  await page.locator('[data-settings-target="about"]').tap();
  await expect(page.locator("#aboutPage")).toBeVisible();
  const scrolled = await page.evaluate(() => {
    const drawer = document.querySelector("#controlsPanel");
    drawer.scrollTop = 600;
    const head = document.querySelector(".drawer-head");
    const tabs = document.querySelector(".settings-tabs");
    const headRect = head.getBoundingClientRect();
    const tabsRect = tabs.getBoundingClientRect();
    return {
      scrollTop: drawer.scrollTop,
      headTop: headRect.top,
      headBottom: headRect.bottom,
      tabsTop: tabsRect.top,
      tabsBottom: tabsRect.bottom,
      headBackground: getComputedStyle(head).backgroundColor,
      tabsBackground: getComputedStyle(tabs).backgroundColor,
    };
  });
  expect(scrolled.scrollTop).toBeGreaterThan(0);
  expect(scrolled.headTop).toBe(0);
  /* The sticky header is tall enough to span the status bar; the tabs stick
   * directly under it (the 1 px border is covered by the header's z-index). */
  expect(scrolled.tabsTop).toBeGreaterThanOrEqual(scrolled.headBottom - 1);
  expect(scrolled.tabsTop).toBeLessThanOrEqual(scrolled.headBottom + 1);
  expect(scrolled.tabsBottom).toBeLessThan(300);
  for (const background of [scrolled.headBackground, scrolled.tabsBackground]) {
    expect(background).not.toContain("rgba");
  }
});

test("a phone drawer fills the viewport instead of leaving a strip of the app visible", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.locator("#panelToggle").tap();
  await expect(page.locator("#controlsPanel")).toBeVisible();
  const metrics = await page.evaluate(() => {
    const drawer = document.querySelector("#controlsPanel");
    const rect = drawer.getBoundingClientRect();
    return { left: rect.left, right: rect.right, width: rect.width, innerWidth };
  });
  /* The Playwright project is a 390 px phone, so the full-width phone rule
   * applies; a tablet keeps the narrower slide-over panel. The sheet slides in
   * over 250 ms, so poll the settled geometry instead of the first frame. */
  expect(metrics.innerWidth).toBeLessThanOrEqual(600);
  await expect.poll(() => page.evaluate(() => {
    const rect = document.querySelector("#controlsPanel").getBoundingClientRect();
    return { left: Math.round(rect.left), right: Math.round(rect.right), width: Math.round(rect.width), innerWidth };
  })).toEqual({ left: 0, right: metrics.innerWidth, width: metrics.innerWidth, innerWidth: metrics.innerWidth });
});

test("the AI settings Save and Clear buttons stay inside the drawer safe area", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#agentButton")).toBeVisible();
  await page.addStyleTag({ content: INSETS });
  await page.locator("#panelToggle").tap();
  await page.locator('[data-settings-target="ai"]').tap();
  await expect(page.locator("#agentSettingsSave")).toBeVisible();
  const metrics = await page.evaluate(() => {
    const drawer = document.querySelector("#controlsPanel");
    drawer.scrollTop = 0;
    const rect = (el) => {
      const r = el.getBoundingClientRect();
      return { top: Math.round(r.top), bottom: Math.round(r.bottom) };
    };
    return {
      save: rect(document.querySelector("#agentSettingsSave")),
      clear: rect(document.querySelector("#agentSettingsClear")),
      drawerBottom: Math.round(drawer.getBoundingClientRect().bottom),
      innerHeight,
    };
  });
  /* The form budget must subtract the status- and navigation-bar insets, or the
   * pinned action row sits past the bottom of the drawer (and the gesture bar)
   * and reads as two missing buttons. */
  for (const button of [metrics.save, metrics.clear]) {
    expect(button.top).toBeGreaterThan(0);
    expect(button.bottom).toBeLessThanOrEqual(metrics.drawerBottom - 36);
  }
});
