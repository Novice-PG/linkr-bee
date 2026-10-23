const STORAGE_KEY = "linkr-terminal-favorite-keys";
export const DEFAULT_FAVORITE_KEYS = ["keyboard", "Escape", "Tab", "modifier:ctrl", "paste", "Enter", "ArrowUp", "ArrowDown"];

export function normalizeFavoriteKeys(value, allowed) {
  if (!Array.isArray(value) || value.some(key => typeof key !== "string")) {
    return DEFAULT_FAVORITE_KEYS.filter(key => allowed.includes(key));
  }
  // The keyboard entry always remains available, even with an empty preference.
  return [...new Set(["keyboard", ...value])].filter(key => allowed.includes(key));
}

export function createTerminalKeyPreferences({ bar, getLang, storage, onCustomize }) {
  const toggle = document.getElementById("terminalKeysToggle");
  const customize = document.getElementById("terminalKeysCustomize");
  const buttons = [...bar.querySelectorAll(".terminal-key-scroll button")];
  const keyOf = button => button.dataset.terminalKey ||
    (button.dataset.modifier ? `modifier:${button.dataset.modifier}` :
      button.id === "pasteTerminalButton" ? "paste" : "keyboard");
  const keys = buttons.map(keyOf);
  let saved;
  try { saved = JSON.parse(storage.getItem(STORAGE_KEY)); } catch { /* Use defaults. */ }
  let favorites = normalizeFavoriteKeys(saved, keys);
  const text = (zh, en) => getLang().startsWith("zh") ? zh : en;
  const dialog = document.createElement("dialog");
  dialog.id = "terminalKeyPreferences";
  dialog.className = "terminal-key-preferences";
  dialog.setAttribute("aria-labelledby", "terminalKeyPreferencesTitle");
  dialog.innerHTML = '<form method="dialog"><h2 id="terminalKeyPreferencesTitle"></h2><p class="field-hint" id="terminalKeyPreferencesHint"></p><div class="terminal-key-choices"></div><p class="field-hint" role="status"></p><div class="terminal-key-preferences-actions"><button class="btn" type="button" data-reset></button><button class="btn btn-primary" value="done" autofocus data-done></button></div></form>';
  document.body.append(dialog);
  const choices = dialog.querySelector(".terminal-key-choices");
  const status = dialog.querySelector('[role="status"]');
  for (const button of buttons) {
    const label = document.createElement("label");
    label.className = "check";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.value = keyOf(button);
    input.disabled = input.value === "keyboard";
    const caption = document.createElement("span");
    label.append(input, caption);
    choices.append(label);
  }
  function apply() {
    for (const button of buttons) {
      const index = favorites.indexOf(keyOf(button));
      button.toggleAttribute("data-favorite-key", index !== -1);
      button.style.setProperty("--favorite-key-order", index === -1 ? "100" : String(index));
    }
    for (const input of choices.querySelectorAll("input")) input.checked = favorites.includes(input.value);
  }
  function persist() {
    apply();
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(favorites));
      status.textContent = "";
    } catch {
      status.textContent = text("本机存储不可用，本次会话仍可使用。", "Storage is unavailable; these choices still work for this session.");
    }
  }
  function collapse() {
    bar.classList.remove("keys-expanded");
    toggle.setAttribute("aria-expanded", "false");
    customize.hidden = true;
    refreshLang();
  }
  function refreshLang() {
    const expanded = toggle.getAttribute("aria-expanded") === "true";
    toggle.textContent = expanded ? text("收起", "Less") : text("更多", "More");
    toggle.title = expanded ? text("仅显示常用按键", "Show favorite keys") : text("显示所有按键", "Show all keys");
    customize.textContent = text("调整常用按键", "Choose favorite keys");
    dialog.querySelector("h2").textContent = customize.textContent;
    dialog.querySelector("#terminalKeyPreferencesHint").textContent = text("选中的按键优先显示，其他按键仍可在“更多”中使用。选择保存在本机。", "Selected keys appear first. All other keys remain in More. Choices stay on this device.");
    dialog.querySelector("[data-reset]").textContent = text("恢复默认", "Restore defaults");
    dialog.querySelector("[data-done]").textContent = text("完成", "Done");
    for (const [index, label] of [...choices.children].entries()) {
      const button = buttons[index];
      label.querySelector("span").textContent = keyOf(button) === "keyboard" ? text("键盘", "Keyboard") : button.textContent.trim();
      const description = button.getAttribute("aria-label");
      if (description) label.querySelector("input").setAttribute("aria-label", description);
    }
  }
  toggle.addEventListener("click", () => {
    const expanded = bar.classList.toggle("keys-expanded");
    toggle.setAttribute("aria-expanded", String(expanded));
    customize.hidden = !expanded;
    refreshLang();
  });
  customize.addEventListener("click", () => {
    status.textContent = "";
    onCustomize?.();
    dialog.showModal();
  });
  choices.addEventListener("change", event => {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) return;
    favorites = normalizeFavoriteKeys(input.checked ? [...favorites, input.value] : favorites.filter(key => key !== input.value), keys);
    persist();
  });
  dialog.querySelector("[data-reset]").addEventListener("click", () => {
    favorites = normalizeFavoriteKeys(null, keys);
    persist();
  });
  // Editing preferences must not trigger terminal/Agent shortcuts underneath.
  dialog.addEventListener("keydown", event => event.stopPropagation());
  apply();
  refreshLang();
  return { refreshLang, collapse };
}
