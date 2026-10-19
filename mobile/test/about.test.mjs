import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { ABOUT_DATA_FILE, collectAbout, renderAboutModule } from "../../tools/gen_about.mjs";

/* The About page states the versions this tree ships with. If a dependency is
 * bumped without regenerating the data file, the page would name the old
 * version — a compliance page that is wrong is worse than none. The test
 * regenerates the artefact in memory and compares it byte for byte. */
const ROOT = fileURLToPath(new URL("../..", import.meta.url));
const DATA_PATH = `${ROOT}${ABOUT_DATA_FILE}`;

test("the committed about data matches the installed tree", () => {
  const committed = readFileSync(DATA_PATH, "utf8");
  assert.equal(committed, renderAboutModule(collectAbout(ROOT)),
    "regenerate with `node tools/gen_about.mjs` after changing dependencies");
});

test("every component is fully described and listed once", () => {
  const data = collectAbout(ROOT);
  const seen = new Set();
  for (const group of data.groups) {
    assert.ok(group.components.length > 0, `${group.id} is empty`);
    for (const component of group.components) {
      assert.ok(component.name, JSON.stringify(component));
      assert.ok(component.version, `${component.name} has no version`);
      assert.ok(component.license && component.license !== "unknown", `${component.name} has no licence`);
      assert.ok(/^https?:\/\//.test(component.url), `${component.name} has no url`);
      assert.ok(!seen.has(component.name), `${component.name} is listed twice`);
      seen.add(component.name);
    }
  }
  // The versions are the installed ones, not the ranges declared in package.json.
  const marked = data.groups.flatMap((group) => group.components).find((c) => c.name === "marked");
  assert.match(marked.version, /^\d+\.\d+\.\d+$/);
  assert.equal(marked.license, "MIT");
});

test("contributors are unique and the CC-BY attribution travels with the artwork", () => {
  const data = collectAbout(ROOT);
  assert.ok(data.contributors.length > 0);
  assert.equal(new Set(data.contributors).size, data.contributors.length);
  const cursors = data.groups.flatMap((group) => group.components).find((c) => c.name === "Macaron cursor theme");
  assert.equal(cursors.license, "CC-BY-4.0");
  assert.match(cursors.author, /UnionTech/);
  assert.ok(existsSync(`${ROOT}web/${cursors.licenseFile}`), "the licence text must ship next to the images");
});

test("the About page is reachable from both navigation surfaces", () => {
  const html = readFileSync(`${ROOT}web/index.html`, "utf8");
  assert.match(html, /data-settings-target="about"/, "the tab strip needs an About entry");
  assert.match(html, /data-rail-target="about"/, "the rail needs an About entry");
  assert.match(html, /data-settings-page="about"/, "the page section is missing");
  for (const id of ["aboutComponents", "aboutContributors", "aboutMeta", "aboutRepository"]) {
    assert.match(html, new RegExp(`id="${id}"`), `${id} is missing`);
  }
  const app = readFileSync(`${ROOT}web/app.js`, "utf8");
  assert.match(app, /\["connection", "terminal", "network", "about"/, "setSettingsPage must accept the page");
  assert.match(app, /aboutPage\?\.refreshLang\(\)/, "the page must re-render on a language switch");
  // Both language tables carry the heading, or the tab falls back to its key.
  const keys = app.match(/aboutNav: "[^"]+"/g) || [];
  assert.equal(keys.length, 2, "aboutNav must exist in both languages");
});

test("the About page renders every group in the data", async () => {
  const data = collectAbout(ROOT);
  const module = await import(DATA_PATH);
  assert.deepEqual(module.ABOUT_DATA, data);
  assert.equal(module.ABOUT_DATA.groups.length, 3, "runtime, firmware and artwork");
});
