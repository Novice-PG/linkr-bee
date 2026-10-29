// Keep native <select> values, form submission and change events as the contract.
// The trigger and listbox supply the same themed surface on desktop and WebViews.
let nextId = 0;
const SVG_NS = 'http://www.w3.org/2000/svg';
function icon(path) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  for (const [key, value] of Object.entries({viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': '1.75', 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': 'true'})) svg.setAttribute(key, value);
  const shape = document.createElementNS(SVG_NS, 'path');
  shape.setAttribute('d', path); svg.append(shape); return svg;
}
function attribute(node, name, value) {
  if (value == null) node.removeAttribute(name);
  else if (node.getAttribute(name) !== String(value)) node.setAttribute(name, String(value));
}

export function installSelectControls(root = document) {
  const controls = new Set();
  const upgraded = new WeakMap();
  let opened = null;
  class SelectControl {
    constructor(select) {
      this.select = select;
      this.id = `${select.id || 'select'}-picker-${++nextId}`;
      this.saved = {tabindex: select.getAttribute('tabindex'), ariaHidden: select.getAttribute('aria-hidden')};
      this.wrapper = document.createElement('span'); this.wrapper.className = 'select-control';
      select.before(this.wrapper); this.wrapper.append(select);
      select.classList.add('select-native'); select.tabIndex = -1; select.setAttribute('aria-hidden', 'true');
      this.trigger = document.createElement('button'); this.trigger.type = 'button';
      this.trigger.id = `${this.id}-button`; this.trigger.className = 'select-trigger';
      this.trigger.tabIndex = this.saved.tabindex == null ? 0 : Number(this.saved.tabindex);
      this.trigger.setAttribute('role', 'combobox'); this.trigger.setAttribute('aria-haspopup', 'listbox');
      this.trigger.setAttribute('aria-controls', this.id); this.trigger.setAttribute('aria-expanded', 'false');
      this.value = document.createElement('span'); this.value.className = 'select-value';
      this.trigger.append(this.value, icon('m7 10 5 5 5-5')); this.wrapper.append(this.trigger);
      this.panel = document.createElement('div'); this.panel.id = this.id; this.panel.className = 'select-listbox';
      this.panel.setAttribute('role', 'listbox');
      if (typeof this.panel.showPopover === 'function') this.panel.setAttribute('popover', 'manual');
      this.panel.hidden = true;
      document.body.append(this.panel);
      this.trigger.addEventListener('click', e => { e.preventDefault(); opened === this ? this.close() : this.open(); });
      this.trigger.addEventListener('keydown', e => this.key(e));
      this.trigger.addEventListener('blur', () => queueMicrotask(() => { if (opened === this && document.activeElement !== this.trigger) this.close(false); }));
      this.panel.addEventListener('mousedown', e => e.preventDefault());
      this.panel.addEventListener('pointermove', e => {
        const row = e.target.closest('.select-option');
        if (e.pointerType !== 'touch' && row && row.getAttribute('aria-disabled') !== 'true') this.activate(Number(row.dataset.index));
      });
      this.panel.addEventListener('click', e => {
        const row = e.target.closest('.select-option'); if (!row) return;
        e.preventDefault(); e.stopPropagation(); this.choose(Number(row.dataset.index));
      });
      this.onChange = () => this.sync();
      this.onFocus = () => this.trigger.focus();
      this.onNativeClick = e => e.preventDefault();
      select.addEventListener('change', this.onChange);
      select.addEventListener('focus', this.onFocus);
      select.addEventListener('click', this.onNativeClick);
      // Existing code assigns select.value/selectedIndex when loading profiles,
      // reverting rejected settings or restoring preferences. Keep those writes
      // synchronous without polling or modifying the global DOM prototype.
      this.properties = [];
      for (const name of ['value', 'selectedIndex']) {
        if (Object.hasOwn(select, name)) continue;
        const native = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, name);
        if (!native?.get || !native?.set) continue;
        Object.defineProperty(select, name, {configurable: true,
          get: () => native.get.call(select),
          set: value => { native.set.call(select, value); this.sync(); }});
        this.properties.push(name);
      }
      this.observer = new MutationObserver(() => this.sync());
      this.observer.observe(select, {childList: true, subtree: true, characterData: true, attributes: true});
      const label = select.labels?.[0];
      this.label = label?.querySelector('.field-label') || label?.querySelector('span') || label;
      if (this.label) this.observer.observe(this.label, {childList: true, subtree: true, characterData: true});
      this.sync();
    }
    records() {
      return Array.from(this.select.options, (option, index) => ({index, value: option.value,
        label: option.label, disabled: option.disabled || (option.parentElement?.tagName === 'OPTGROUP' && option.parentElement.disabled),
        hidden: option.hidden || option.parentElement?.hidden,
        group: option.parentElement?.tagName === 'OPTGROUP' ? option.parentElement.label : ''})).filter(option => !option.hidden);
    }
    sync() {
      const select = this.select;
      const chosen = select.options[select.selectedIndex];
      const text = chosen?.label || '';
      if (this.value.textContent !== text) this.value.textContent = text;
      this.trigger.disabled = select.matches(':disabled');
      this.wrapper.hidden = select.hidden;
      const directLabel = select.getAttribute('aria-label');
      let labelledBy = select.getAttribute('aria-labelledby');
      if (!directLabel && !labelledBy && this.label) {
        if (!this.label.id) this.label.id = `${this.id}-label`;
        labelledBy = this.label.id;
      }
      attribute(this.trigger, 'aria-label', directLabel || (!labelledBy ? text : null));
      attribute(this.trigger, 'aria-labelledby', directLabel ? null : labelledBy);
      for (const name of ['aria-describedby', 'aria-invalid', 'aria-required']) attribute(this.trigger, name, select.getAttribute(name));
      attribute(this.panel, 'aria-label', directLabel);
      attribute(this.panel, 'aria-labelledby', directLabel ? null : labelledBy);
      if (opened === this) {
        if (this.trigger.disabled || this.wrapper.hidden || !select.isConnected || !this.trigger.getClientRects().length) return this.close(false);
        const signature = JSON.stringify(this.records());
        if (signature !== this.signature) this.build();
        for (const row of this.panel.querySelectorAll('.select-option')) attribute(row, 'aria-selected', Number(row.dataset.index) === select.selectedIndex);
        this.position();
      }
    }
    build() {
      this.options = this.records(); this.signature = JSON.stringify(this.options);
      this.panel.replaceChildren(); let group = null, parent = this.panel;
      for (const option of this.options) {
        if (option.group !== group) {
          group = option.group; parent = this.panel;
          if (group) {
            parent = document.createElement('div'); parent.setAttribute('role', 'group'); parent.setAttribute('aria-label', group);
            const title = document.createElement('div'); title.className = 'select-group-label'; title.textContent = group; title.setAttribute('aria-hidden', 'true');
            parent.append(title); this.panel.append(parent);
          }
        }
        const row = document.createElement('div'); row.id = `${this.id}-option-${option.index}`;
        row.className = 'select-option'; row.dataset.index = option.index; row.setAttribute('role', 'option');
        row.setAttribute('aria-selected', String(option.index === this.select.selectedIndex));
        row.setAttribute('aria-disabled', String(Boolean(option.disabled)));
        const label = document.createElement('span'); label.className = 'select-option-label'; label.textContent = option.label;
        const check = icon('m5 12 4 4L19 6'); check.classList.add('select-option-check'); row.append(label, check); parent.append(row);
      }
      if (!this.options.length) {
        const empty = document.createElement('div'); empty.className = 'select-empty';
        empty.textContent = document.documentElement.lang.startsWith('zh') ? '暂无可用选项' : 'No options available'; this.panel.append(empty);
      }
      const selected = this.options.find(option => option.index === this.select.selectedIndex && !option.disabled);
      this.active = selected?.index ?? this.options.find(option => !option.disabled)?.index ?? -1;
      this.activate(this.active);
    }
    open() {
      if (this.select.matches(':disabled') || this.select.hidden) return;
      opened?.close(false); opened = this; this.trigger.focus(); this.build();
      this.panel.hidden = false;
      if (typeof this.panel.showPopover === 'function') this.panel.showPopover();
      this.trigger.setAttribute('aria-expanded', 'true'); this.position(); this.reveal();
    }
    close(focus = true) {
      if (opened === this) opened = null;
      this.trigger.setAttribute('aria-expanded', 'false'); this.trigger.removeAttribute('aria-activedescendant');
      if (typeof this.panel.hidePopover === 'function' && this.panel.matches(':popover-open')) this.panel.hidePopover();
      this.panel.hidden = true; this.search = '';
      if (focus && this.trigger.isConnected && !this.trigger.disabled) {
        const target = document.getElementById(this.select.dataset.focusTarget || "") || this.trigger;
        target.focus({preventScroll: true});
      }
    }
    position() {
      const anchor = this.wrapper.closest(".suggestion-control") || this.trigger;
      const r = anchor.getBoundingClientRect(), viewport = window.visualViewport;
      const x = viewport?.offsetLeft || 0, y = viewport?.offsetTop || 0;
      const w = viewport?.width || window.innerWidth, h = viewport?.height || window.innerHeight;
      if (!r.width || !r.height || r.bottom < y || r.top > y + h) return this.close(false);
      const width = Math.min(Math.max(r.width, 160), w - 16);
      this.panel.style.width = `${width}px`;
      const below = y + h - r.bottom - 14, above = r.top - y - 14;
      const desired = Math.min(this.panel.scrollHeight + 2, 320, h - 16);
      const up = below < Math.min(desired, 160) && above > below;
      const height = Math.min(desired, Math.max(36, up ? above : below));
      this.panel.style.maxHeight = `${height}px`;
      this.panel.style.left = `${Math.max(x + 8, Math.min(r.left, x + w - width - 8))}px`;
      this.panel.style.top = `${Math.max(y + 8, Math.min(up ? r.top - height - 6 : r.bottom + 6, y + h - height - 8))}px`;
    }
    activate(index) {
      this.active = index;
      for (const row of this.panel.querySelectorAll('.select-option')) row.classList.toggle('is-active', Number(row.dataset.index) === index);
      if (index >= 0) this.trigger.setAttribute('aria-activedescendant', `${this.id}-option-${index}`);
      else this.trigger.removeAttribute('aria-activedescendant');
    }
    reveal() {
      const row = this.panel.querySelector(`[data-index="${this.active}"]`); if (!row) return;
      if (row.offsetTop < this.panel.scrollTop) this.panel.scrollTop = row.offsetTop;
      else if (row.offsetTop + row.offsetHeight > this.panel.scrollTop + this.panel.clientHeight) this.panel.scrollTop = row.offsetTop + row.offsetHeight - this.panel.clientHeight;
    }
    choose(index) {
      const option = this.options.find(item => item.index === index);
      if (!option || option.disabled || this.select.matches(':disabled')) return;
      const changed = this.select.selectedIndex !== index;
      this.close(); this.select.selectedIndex = index;
      if (changed) {
        this.select.dispatchEvent(new Event('input', {bubbles: true}));
        this.select.dispatchEvent(new Event('change', {bubbles: true}));
      }
      this.sync();
    }
    key(event) {
      const key = event.key;
      if (event.ctrlKey || event.metaKey || event.altKey) return;
      if (opened !== this) {
        if (['Enter', ' ', 'ArrowDown', 'ArrowUp'].includes(key)) { event.preventDefault(); event.stopPropagation(); this.open(); }
        else if (key.length === 1) this.open();
        else return;
        if (key === 'Enter' || key === ' ') return;
      }
      if (key === 'Tab') { this.close(false); return; }
      if (key === 'Escape') { event.preventDefault(); event.stopPropagation(); this.close(); return; }
      if (key === 'Enter' || key === ' ') { event.preventDefault(); event.stopPropagation(); this.choose(this.active); return; }
      const enabled = this.options.filter(option => !option.disabled), at = enabled.findIndex(option => option.index === this.active);
      let next;
      if (key === 'ArrowDown') next = Math.min(at + 1, enabled.length - 1);
      else if (key === 'ArrowUp') next = Math.max(0, at - 1);
      else if (key === 'Home') next = 0;
      else if (key === 'End') next = enabled.length - 1;
      else if (key === 'PageDown') next = Math.min(at + 5, enabled.length - 1);
      else if (key === 'PageUp') next = Math.max(0, at - 5);
      else if (key.length === 1) {
        const now = Date.now(); this.search = (now - (this.searchAt || 0) < 700 ? this.search || '' : '') + key.toLocaleLowerCase(); this.searchAt = now;
        next = enabled.findIndex(option => option.label.toLocaleLowerCase().startsWith(this.search));
      } else return;
      event.preventDefault(); event.stopPropagation();
      if (next >= 0 && enabled[next]) { this.activate(enabled[next].index); this.reveal(); }
    }
    dispose() {
      if (opened === this) this.close(false);
      this.observer.disconnect(); this.panel.remove();
      this.select.removeEventListener('change', this.onChange); this.select.removeEventListener('focus', this.onFocus); this.select.removeEventListener('click', this.onNativeClick);
      for (const name of this.properties) delete this.select[name];
      this.select.classList.remove('select-native');
      attribute(this.select, 'aria-hidden', this.saved.ariaHidden); attribute(this.select, 'tabindex', this.saved.tabindex);
      this.wrapper.replaceWith(this.select); controls.delete(this); upgraded.delete(this.select);
    }
  }
  const suggestions = new Map();
  function enhanceSuggestions(input) {
    const listId = input.getAttribute('list'), list = document.getElementById(listId);
    if (!list || list.tagName !== 'DATALIST' || suggestions.has(input)) return;
    const wrapper = document.createElement('span'); wrapper.className = 'suggestion-control';
    input.before(wrapper); wrapper.append(input); input.removeAttribute('list');
    const select = document.createElement('select'); select.className = 'suggestion-select';
    select.id = `${input.id || 'suggestion'}-choices-${++nextId}`;
    select.dataset.focusTarget = input.id;
    wrapper.append(select);
    const label = input.labels?.[0];
    const labelText = label?.querySelector('.field-label') || label;
    function refresh() {
      const values = [...new Set(Array.from(list.options, option => option.value).filter(Boolean))];
      const previous = Array.from(select.options, option => option.value).slice(1);
      if (JSON.stringify(values) !== JSON.stringify(previous)) {
        const placeholder = new Option('', ''); placeholder.disabled = true; placeholder.hidden = true;
        select.replaceChildren(placeholder, ...values.map(value => new Option(value, value)));
      }
      select.value = values.includes(input.value) ? input.value : '';
      select.disabled = input.disabled || input.readOnly; select.hidden = values.length === 0;
      const name = input.getAttribute('aria-label') || labelText?.textContent?.trim() || input.placeholder || '';
      attribute(select, 'aria-label', `${name} · ${document.documentElement.lang.startsWith('zh') ? '选择建议' : 'Suggestions'}`);
      attribute(select, 'aria-describedby', input.getAttribute('aria-describedby'));
    }
    const choose = () => {
      if (!select.value || input.disabled || input.readOnly) return;
      input.value = select.value;
      input.dispatchEvent(new Event('input', {bubbles: true}));
      input.dispatchEvent(new Event('change', {bubbles: true}));
      input.focus({preventScroll: true});
    };
    const key = event => {
      if (event.key !== 'ArrowDown' || event.isComposing || event.ctrlKey || event.metaKey || event.altKey || select.hidden || input.disabled || input.readOnly) return;
      const trigger = wrapper.querySelector('.select-trigger'); if (!trigger) return;
      event.preventDefault(); event.stopPropagation(); trigger.click();
    };
    wrapper.addEventListener('pointerdown', refresh, true); wrapper.addEventListener('keydown', refresh, true);
    select.addEventListener('change', choose); input.addEventListener('input', refresh); input.addEventListener('change', refresh); input.addEventListener('keydown', key);
    const watch = new MutationObserver(refresh);
    watch.observe(list, {childList: true, subtree: true, attributes: true, characterData: true});
    watch.observe(input, {attributes: true, attributeFilter: ['disabled', 'readonly', 'aria-label']});
    if (labelText) watch.observe(labelText, {childList: true, subtree: true, characterData: true});
    const dispose = () => {
      watch.disconnect(); input.removeEventListener('input', refresh); input.removeEventListener('change', refresh); input.removeEventListener('keydown', key);
      wrapper.removeEventListener('pointerdown', refresh, true); wrapper.removeEventListener('keydown', refresh, true);
      select.removeEventListener('change', choose); select.remove(); input.setAttribute('list', listId);
      wrapper.replaceWith(input); suggestions.delete(input);
    };
    suggestions.set(input, {refresh, dispose}); refresh();
  }
  function discover() {
    for (const input of root.querySelectorAll("input[list]")) enhanceSuggestions(input);
    for (const [input, widget] of suggestions) if (!root.contains(input)) widget.dispose();
    for (const select of root.querySelectorAll('select:not([multiple])')) {
      if (select.size > 1 || upgraded.has(select)) continue;
      const control = new SelectControl(select); upgraded.set(select, control); controls.add(control);
    }
    for (const control of controls) if (!root.contains(control.select)) control.dispose();
    if (opened) opened.sync();
  }
  const observer = new MutationObserver(discover);
  discover(); observer.observe(root, {childList: true, subtree: true, attributes: true, attributeFilter: ['hidden', 'disabled']});
  const outside = event => { if (opened && !opened.wrapper.contains(event.target) && !opened.panel.contains(event.target)) opened.close(false); };
  let positionFrame = 0;
  const resize = () => {
    if (!opened || positionFrame) return;
    positionFrame = requestAnimationFrame(() => { positionFrame = 0; opened?.position(); });
  };
  // Drawer opening, focus and async form content can scroll the owner just
  // after a click. Follow the anchor rather than dismissing that first click.
  const scroll = event => { if (opened && !opened.panel.contains(event.target)) resize(); };
  const escape = event => { if (opened && event.key === 'Escape') { event.preventDefault(); event.stopImmediatePropagation(); opened.close(); } };
  const reset = () => queueMicrotask(() => { for (const control of controls) control.sync(); });
  document.addEventListener('pointerdown', outside, true); document.addEventListener('scroll', scroll, true);
  window.addEventListener('keydown', escape, true); window.addEventListener('resize', resize);
  window.visualViewport?.addEventListener('resize', resize); root.addEventListener('reset', reset);
  return { refresh() { discover(); for (const control of controls) control.sync(); }, destroy() {
    observer.disconnect(); if (positionFrame) cancelAnimationFrame(positionFrame);
    for (const control of [...controls]) control.dispose();
    for (const widget of [...suggestions.values()]) widget.dispose();
    document.removeEventListener('pointerdown', outside, true); document.removeEventListener('scroll', scroll, true);
    window.removeEventListener('keydown', escape, true); window.removeEventListener('resize', resize);
    window.visualViewport?.removeEventListener('resize', resize); root.removeEventListener('reset', reset);
  }};
}
