/*
 * UI review screenshots: the board page in every supported presentation.
 *
 * A frontend change is not reviewable from code alone. This script captures the
 * same surface at phone, tablet and desktop widths, in both themes and both
 * languages, from the hardware-free demo (`?demo=board`). Compare the output
 * with DESIGN.md before calling a UI change done.
 *
 * Usage, from the repository root:
 *   sh tools/serve_web.sh 8090            # in another terminal
 *   cd mobile && node ui-review.mjs       # optional URL argument
 *
 * Screenshots land in build-demo/ui-review/ (gitignored).
 */
import { chromium } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const base = process.argv[2] || "http://127.0.0.1:8090";
const outDir = new URL("../build-demo/ui-review/", import.meta.url);

const viewports = [
  { name: "small-phone", width: 338, height: 844, isMobile: true, hasTouch: true },
  { name: "phone", width: 390, height: 844, isMobile: true, hasTouch: true },
  { name: "tablet", width: 834, height: 1000, isMobile: true, hasTouch: true },
  { name: "desktop", width: 1440, height: 900, isMobile: false, hasTouch: false },
];

const appearances = [
  { lang: "zh", theme: "light" },
  { lang: "zh", theme: "dark" },
  { lang: "en", theme: "light" },
  { lang: "en", theme: "dark" },
];

await mkdir(outDir, { recursive: true });
const browser = await chromium.launch();
try {
  for (const viewport of viewports) {
    for (const appearance of appearances) {
      const context = await browser.newContext({
        viewport: { width: viewport.width, height: viewport.height },
        isMobile: viewport.isMobile,
        hasTouch: viewport.hasTouch,
        deviceScaleFactor: 2,
      });
      await context.addInitScript(({ lang, theme }) => {
        localStorage.setItem("linkr-lang", lang);
        localStorage.setItem("linkr-theme", theme);
        localStorage.removeItem("linkr-demo-prompt");
      }, appearance);
      const page = await context.newPage();
      await page.goto(`${base}/?demo=board`);
      await page.waitForFunction(() => document.querySelector('[data-settings-page="board"]:not([hidden])'));
      await page.waitForTimeout(1800);
      const file = new URL(
        `${appearance.lang}-${appearance.theme}-${viewport.name}.png`,
        outDir,
      );
      await page.screenshot({ path: fileURLToPath(file), fullPage: false });
      console.log(`captured ${fileURLToPath(file)}`);
      await context.close();
    }
  }
} finally {
  await browser.close();
}
