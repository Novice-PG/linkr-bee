import { readFileSync } from "node:fs";
import { test, expect } from "@playwright/test";

const stylesheet = readFileSync(new URL("../../web/style.css", import.meta.url), "utf8");

const heightLayouts = [
  { name: "keyboard workspace", classes: "soft-keyboard-open", width: 800,
    body: '<main class="app-shell"></main>', selector: ".app-shell", inset: 0 },
  { name: "fullscreen fallback", classes: "soft-keyboard-open", width: 800,
    body: '<section class="terminal-card terminal-fullscreen-fallback"></section>', selector: ".terminal-card", inset: 0 },
  { name: "Agent workspace", classes: "agent-mode settings-drawer-layout", width: 800,
    body: '<main class="app-shell"></main>', selector: ".app-shell", inset: 0 },
  ...[390, 800].map(width => ({ name: `settings drawer ${width}px`, classes: "settings-drawer-layout", width,
    body: '<main class="app-shell sidebar-open"><aside class="controls"></aside></main>',
    selector: ".controls", inset: width <= 600 ? 0 : 24 })),
];

for (const supportsDynamicViewport of [false, true]) {
  for (const layout of heightLayouts) {
    for (const visibleHeight of [null, 500]) {
      test(`${layout.name} height fallback: var=${visibleHeight ?? "unset"}, dvh=${supportsDynamicViewport}`, async ({ page }) => {
        await page.setViewportSize({ width: layout.width, height: 800 });
        // No app bootstrap: exercise the CSS fallback before JS supplies a height.
        await page.setContent(`<html class="${layout.classes}"><head><meta name="viewport" content="width=device-width,initial-scale=1"></head><body>${layout.body}</body></html>`);
        if (visibleHeight !== null) await page.evaluate(height => {
          document.documentElement.style.setProperty("--terminal-viewport-height", `${height}px`);
        }, visibleHeight);
        await page.addStyleTag({ content: supportsDynamicViewport ? stylesheet : stylesheet.replaceAll("dvh", "unsupportedunit") });
        const height = await page.locator(layout.selector).evaluate(element => ({
          pixels: getComputedStyle(element).height,
          // Fixed inset:0 can stretch auto to the same size. Check that the
          // declared fallback actually resolves, rather than accepting auto.
          computed: element.computedStyleMap().get("height").toString(),
        }));
        const expected = `${(visibleHeight ?? 800) - layout.inset}px`;
        expect(height).toEqual({ pixels: expected, computed: expected });
      });
    }
  }
  test(`fullscreen keyboard padding survives dvh support=${supportsDynamicViewport}`, async ({ page }) => {
    await page.setViewportSize({ width: 800, height: 800 });
    await page.setContent('<html class="soft-keyboard-open"><head><meta name="viewport" content="width=device-width,initial-scale=1"></head><body><div class="terminal-card test-fullscreen"></div></body></html>');
    // Exercise the shipped cascade. An unknown unit models old WebViews,
    // including var() declarations that parse but fail at computed-value time.
    const css = stylesheet.replaceAll(":fullscreen", ".test-fullscreen");
    await page.addStyleTag({ content: supportsDynamicViewport ? css : css.replaceAll("dvh", "unsupportedunit") });
    for (const [visibleHeight, expectedPadding] of [[null, "0px"], [500, "300px"], [800, "0px"]]) {
      await page.evaluate(height => {
        if (height === null) document.documentElement.style.removeProperty("--terminal-viewport-height");
        else document.documentElement.style.setProperty("--terminal-viewport-height", `${height}px`);
      }, visibleHeight);
      await expect(page.locator(".terminal-card")).toHaveCSS("padding-bottom", expectedPadding);
    }
  });
}
