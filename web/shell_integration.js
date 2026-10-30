// Linkr uses an unknown OSC 633 property, not C/D without a nonce. Other
// terminals ignore the property; no clipboard, title or notification OSC is used.
export const SHELL_OSC_PREFIX = '633;P;Linkr=1:';
export const SHELL_FUNCTION = '__linkr_run_v1';
const NONCE = /^[a-f0-9]{32}$/;
export const shellQuote = value => "'" + value.replaceAll("'", "'\\''") + "'";
export function parseShellMarker(body) {
  const match = body.match(/^633;P;Linkr=1:([a-f0-9]{32}):(0|[1-9][0-9]{0,8}):(R|C|D)(?::([0-9]{1,3}))?$/);
  if (!match || (match[3] === 'D') !== (match[4] !== undefined) || Number(match[4] || 0) > 255) return null;
  if ((match[3] === 'R') !== (Number(match[2]) === 0)) return null;
  return { nonce: match[1], sequence: Number(match[2]), phase: match[3], ...(match[4] !== undefined ? {exitCode: Number(match[4])} : {}) };
}
export function shellMarker(nonce, sequence, phase, exitCode) {
  const body = `${SHELL_OSC_PREFIX}${nonce}:${sequence}:${phase}${exitCode === undefined ? '' : ':' + exitCode}`;
  if (!parseShellMarker(body)) throw new Error('Invalid shell marker.');
  return '\x1b]' + body + '\x07';
}
export function shellIntegrationCommand(nonce) {
  if (!NONCE.test(nonce)) throw new Error('Invalid shell integration nonce.');
  // Own only this reserved namespace. Do not overwrite an unrelated function,
  // install a file, change stty/PROMPT_COMMAND or touch shell startup scripts.
  return `if command -v ${SHELL_FUNCTION} >/dev/null 2>&1 && [ "\${__linkr_protocol-}" != linkr-shell-v1 ]; then command printf '%s\\n' 'Linkr: reserved shell function already exists'; else __linkr_protocol=linkr-shell-v1; __linkr_nonce=${nonce}; __linkr_seq=0; ${SHELL_FUNCTION}() { [ "$#" -eq 1 ] || return 125; __linkr_seq=$((__linkr_seq + 1)); command printf '\\030\\033]633;P;Linkr=1:%s:%s:C\\007' "$__linkr_nonce" "$__linkr_seq"; if command sh -c "$1"; then __linkr_rc=0; else __linkr_rc=$?; fi; command printf '\\n\\030\\033]633;P;Linkr=1:%s:%s:D:%s\\007' "$__linkr_nonce" "$__linkr_seq" "$__linkr_rc"; return 0; }; command printf '\\030\\033]633;P;Linkr=1:%s:0:R\\007' "$__linkr_nonce"; fi`;
}

// Parse decoded raw UTF-16 positions before SerialJournal masks control strings.
// The payload is bounded; nested/unfinished controls never become marker events.
export function createShellMarkerParser() {
  let state = 'text', body = '', start = 0, oversized = false;
  function begin(position) { body = ''; start = position; oversized = false; state = 'osc'; }
  return {
    push(text, offset) {
      const events = [];
      function finish(end) {
        const marker = !oversized && parseShellMarker(body);
        if (marker) {
          events.push({...marker, start, end});
          if (events.length > 128) events.shift();
        }
        state = 'text'; body = '';
      }
      for (let i = 0; i < text.length; i++) {
        const char = text[i], position = offset + i;
        if (char === '\x18' || char === '\x1a') { state = 'text'; body = ''; continue; }
        if (state === 'osc') {
          if (char === '\x07' || char === '\x9c') finish(position + 1);
          else if (char === '\x1b') state = 'osc-escape';
          else if (body.length < 160) body += char;
          else oversized = true;
        } else if (state === 'osc-escape') {
          if (char === '\\') finish(position + 1);
          else { state = 'text'; body = ''; }
        } else if (state === 'string') {
          if (char === '\x9c') state = 'text';
          else if (char === '\x1b') state = 'string-escape';
        } else if (state === 'string-escape') {
          state = char === '\\' ? 'text' : 'string';
        } else if (state === 'escape') {
          if (char === ']') begin(start);
          else if ('PX^_'.includes(char)) state = 'string';
          else if (char === '[') state = 'csi';
          else state = 'text';
        } else if (state === 'csi') {
          if (char >= '@' && char <= '~') state = 'text';
        } else if (char === '\x1b') { state = 'escape'; start = position; }
        else if (char === '\x9d') begin(position);
        else if ('\x90\x98\x9e\x9f'.includes(char)) state = 'string';
      }
      return events;
    },
  };
}
