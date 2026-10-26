import { test, expect } from "@playwright/test";

/* xterm 6 paints its own scrollbar and sizes it in JavaScript from the
 * overviewRuler option, not from CSS. These assertions therefore read the
 * inline width xterm applied and the computed thumb inset. */

async function openTerminal(page) {
  await page.goto("/");
  await page.waitForSelector(".xterm-scrollable-element > .scrollbar.vertical");
}

async function scrollbarMetrics(page) {
  return page.evaluate(() => {
    const bar = document.querySelector(".xterm-scrollable-element > .scrollbar.vertical");
    const slider = bar.querySelector(".slider");
    const ruler = document.querySelector(".xterm-decoration-overview-ruler");
    const style = getComputedStyle(slider);
    return {
      track: Number.parseFloat(bar.style.width),
      slider: slider.getBoundingClientRect().width,
      border: Number.parseFloat(style.borderLeftWidth),
      radius: style.borderRadius,
      backgroundClip: style.backgroundClip,
      rulerHidden: !ruler || getComputedStyle(ruler).display === "none",
    };
  });
}

test("the touch terminal keeps a narrow, inset scrollbar", async ({ page }) => {
  await openTerminal(page);
  const metrics = await scrollbarMetrics(page);
  expect(metrics.track).toBe(8);
  // xterm writes the track width onto the slider too; the transparent border
  // is what leaves a 4 px visible thumb inside that hit area.
  expect(metrics.slider).toBe(8);
  expect(metrics.border).toBe(2);
  expect(metrics.radius).toBe("999px");
  expect(metrics.backgroundClip).toBe("padding-box");
  // The unused overview-ruler canvas must not draw over the track.
  expect(metrics.rulerHidden).toBe(true);
});

test.describe("pointer-fine host", () => {
  test.use({ viewport: { width: 1280, height: 800 }, isMobile: false, hasTouch: false });

  test("the desktop terminal stays at its previous 10 px track", async ({ page }) => {
    await openTerminal(page);
    const metrics = await scrollbarMetrics(page);
    expect(metrics.track).toBe(10);
    expect(metrics.slider).toBe(10);
    expect(metrics.border).toBe(2);
    expect(metrics.rulerHidden).toBe(true);
  });
});
