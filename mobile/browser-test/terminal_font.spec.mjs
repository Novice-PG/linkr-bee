import { test, expect } from "@playwright/test";

/* The terminal renders with xterm's DOM renderer, so it depends on CSS font
 * fallback: desktop monospace stacks have no glyphs for the Unicode 13/16
 * legacy-computing blocks used by ANSI art. The bundled subset is the only
 * thing that turns those cells from .notdef boxes into shapes; if it fails to
 * load (or the family drops out of the stack) the terminal silently regresses
 * to tofu, which no other test would notice. */
test("legacy computing glyphs render through the bundled fallback font", async ({ page }) => {
  const fontResponses = [];
  page.on("response", (response) => {
    if (response.url().includes("linkr-legacy-computing.woff2")) {
      fontResponses.push(response.status());
    }
  });

  await page.goto("/");
  await expect(page.locator("#terminalOutput .xterm")).toBeVisible();

  const result = await page.evaluate(async () => {
    const samples = ["\u{1cc1a}", "\u{1fbe9}", "\u{1fb00}"];
    const text = samples.join("");
    await document.fonts.load('32px "Linkr Legacy Computing"', text);
    const loaded = document.fonts.check('32px "Linkr Legacy Computing"', text);

    const canvas = document.createElement("canvas");
    canvas.width = 40;
    canvas.height = 40;
    const ctx = canvas.getContext("2d");
    const render = (char) => {
      ctx.clearRect(0, 0, 40, 40);
      ctx.fillStyle = "#000";
      ctx.fillRect(0, 0, 40, 40);
      ctx.fillStyle = "#fff";
      ctx.font = '32px "Linkr Legacy Computing"';
      ctx.textBaseline = "alphabetic";
      ctx.fillText(char, 2, 34);
      const data = ctx.getImageData(0, 0, 40, 40).data;
      let hash = 2166136261;
      let ink = 0;
      for (let i = 0; i < data.length; i += 4) {
        if (data[i] > 128) ink += 1;
        hash = Math.imul(hash ^ data[i], 16777619) >>> 0;
      }
      return { hash, ink };
    };
    const notdef = render("\u{10ffff}");
    return {
      loaded,
      notdef,
      glyphs: samples.map((char) => render(char)),
    };
  });

  expect(result.loaded).toBe(true);
  expect(fontResponses, "the fallback font must actually be fetched").toContain(200);
  for (const glyph of result.glyphs) {
    expect(glyph.ink, "a covered glyph must draw ink").toBeGreaterThan(8);
    expect(glyph.hash, "a covered glyph must differ from .notdef").not.toBe(result.notdef.hash);
  }
});
