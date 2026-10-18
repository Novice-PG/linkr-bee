/* Icon rail → settings drawer.
 *
 * The rail mirrors the tab strip but is deliberately NOT one of the tabs:
 * app.js builds its tab list from [data-settings-target] and resolves "the AI
 * tab" with querySelector, which returns the first match in document order.
 * A rail button carrying that attribute would shadow the real tab. So this
 * module forwards clicks to the real tab and mirrors .active back onto the rail.
 */

const shell = document.querySelector(".app-shell");
const railButtons = Array.from(document.querySelectorAll("[data-rail-target]"));
const tabs = Array.from(
  document.querySelectorAll(".settings-tab[data-settings-target]"),
);

function panelIsOpen() {
  if (document.documentElement.classList.contains("settings-drawer-layout")) {
    return shell.classList.contains("sidebar-open");
  }
  return !shell.classList.contains("controls-hidden");
}

function tabFor(name) {
  return tabs.find((tab) => tab.dataset.settingsTarget === name) || null;
}

railButtons.forEach((button) => {
  button.addEventListener("click", () => {
    const tab = tabFor(button.dataset.railTarget);
    if (!tab || tab.hidden) return;
    tab.click();
    // Selecting a section with the drawer collapsed should also reveal it.
    if (!panelIsOpen()) document.getElementById("panelToggle")?.click();
  });
});

let queued = false;

function sync() {
  queued = false;
  railButtons.forEach((button) => {
    const tab = tabFor(button.dataset.railTarget);
    const active = Boolean(tab && tab.classList.contains("active"));
    button.classList.toggle("active", active);
    // aria-selected only applies to tabs/options; these are navigation
    // buttons, so the current section is expressed with aria-current.
    if (active) {
      button.setAttribute("aria-current", "page");
    } else {
      button.removeAttribute("aria-current");
    }
    // app.js toggles [hidden] on the assistant tab when the feature is off.
    if (button.dataset.railTarget === "ai") {
      button.classList.toggle("is-available", Boolean(tab && !tab.hidden));
    }
  });
}

function scheduleSync() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(sync);
}

const panel = document.getElementById("controlsPanel");
if (panel) {
  new MutationObserver(scheduleSync).observe(panel, {
    subtree: true,
    attributes: true,
    attributeFilter: ["class", "hidden"],
  });
}

sync();
// app.js applies the persisted page and language after its own init pass.
setTimeout(sync, 120);
setTimeout(sync, 600);
