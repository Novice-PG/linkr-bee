export const TARGET_DIRECTORY_SETUP = 'sudo install -d -m 0755 -o "$(id -u)" -g "$(id -g)" /var/lib/linkr';
export const TARGET_ID_PATH = '/var/lib/linkr/device-id';
export const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
export function bindingReply(text) {
  const match = String(text).trim().match(/^OK target=(none|[0-9a-f-]{36})$/);
  if (!match || (match[1] !== 'none' && !UUID_PATTERN.test(match[1]))) throw new Error('Bee binding unsupported or invalid response; update firmware.');
  return match[1] === 'none' ? null : match[1];
}
const quoteShell = value => "'" + value.replaceAll("'", "'\\''") + "'";
export function targetIdentityCommand(action, uuid, marker) {
  if (!['verify','bind','regenerate'].includes(action) || !UUID_PATTERN.test(uuid) || !/^LINKR_ID_[a-f0-9]+$/.test(marker)) throw new Error('Invalid identity request');
  const write = action === 'verify' ? '' : `${action === 'bind' ? 'if test ! -e "$p"; then ' : ''}mkdir -p "$d"; t=$(mktemp "$d/.id.XXXXXX"); trap 'rm -f "$t"' EXIT; printf '%s\\n' "$2" > "$t"; chmod 644 "$t"; ${action === 'bind' ? 'ln "$t" "$p"; fi;' : 'mv -f "$t" "$p";'}`;
  const operation = `set -eu; p=$1; d=\${p%/*}; test ! -L "$d"; test ! -L "$p"; ${write}test -f "$p"; id=$(cat "$p"); printf '\\n%s:%s\\n%s_PATH:%s\\n' "$3" "$id" "$3" "$p"`;
  const run = `sh -c "$op" sh`;
  const args = `${quoteShell(uuid)} ${quoteShell(marker)}`;
  const existing = `if test -e "$s" || test -L "$s"; then ${run} "$s" ${args}; exit $?; fi; if test -e "$u" || test -L "$u"; then ${run} "$u" ${args}; exit $?; fi;`;
  // Existing identities always win. An inaccessible/invalid system ID must not
  // silently become a new user identity. sudo only runs this fixed ID operation.
  const existingSystem = action === 'regenerate'
    ? existing.replace(`${run} "$s" ${args};`, `if test -w "\${s%/*}"; then ${run} "$s" ${args}; else sudo -p '[sudo] password: ' ${run} "$s" ${args}; fi;`)
    : existing;
  const create = action === 'verify' ? 'exit 1;' : `if test -w "\${s%/*}" || test "$(id -u)" = 0; then ${run} "$s" ${args} && exit 0; elif command -v sudo >/dev/null 2>&1; then sudo -p '[sudo] password: ' ${run} "$s" ${args} && exit 0; fi; test ! -e "$s" && test ! -L "$s" || exit 1; ${run} "$u" ${args};`;
  return `op=${quoteShell(operation)}; s=${TARGET_ID_PATH}; u="\${HOME:?}/.local/share/linkr/device-id"; ${existingSystem} ${create}`;
}
export function observedTargetPath(record, marker) {
  const matches = [...String(record.evidence || '').matchAll(new RegExp(`(?:^|\\n)${marker}_PATH:([^\\r\\n]+)\\r?(?=\\n)`, 'g'))];
  return matches.length === 1 ? matches[0][1] : null;
}
export function observedTargetId(record, marker) {
  if (record.executionStatus === 'completed' && record.exitCode !== 0 && /permission denied|权限不够|权限不足|操作不允许/i.test(record.evidence || '')) {
    const error = new Error('Target directory permission denied.');
    error.code = 'TARGET_PERMISSION_DENIED';
    throw error;
  }
  if (record.executionStatus !== 'completed' || record.exitCode !== 0 || record.evidenceTruncated) throw new Error('Target identity command failed or is unverified; inspect the terminal. Root permissions may be required.');
  const matches=[...record.evidence.matchAll(new RegExp(`(?:^|\\n)${marker}:([0-9a-f-]+)\\r?(?=\\n)`, 'g'))];
  if (matches.length !== 1 || !UUID_PATTERN.test(matches[0][1])) throw new Error('Invalid or missing target UUID. Existing file was not automatically overwritten.');
  return matches[0][1];
}
