export const PROFILE_PROBE = `printf 'LINKR_PROFILE_BEGIN\\n'; uname -a; printf 'LINKR_OS\\n'; cat /etc/os-release 2>/dev/null; printf 'LINKR_MODEL\\n'; cat /proc/device-tree/model 2>/dev/null; printf '\\nLINKR_BOOT\\n'; cat /proc/sys/kernel/random/boot_id 2>/dev/null; printf 'LINKR_DISK\\n'; df -Pk /; printf 'LINKR_TOOLS\\n'; for t in sh curl wget sha256sum shasum openssl sudo systemctl busybox; do command -v "$t" >/dev/null 2>&1 && printf 'TOOL:%s\\n' "$t"; done; printf 'LINKR_PROFILE_END\\n'`;
export function parseDeviceProfile(text) {
  // UART shells can emit LF-CR (including before the first marker), not
  // only LF or CR-LF. Normalize before matching the record boundaries.
  const normalized = text.replace(/\r/g, '');
  const match = normalized.match(/(?:^|\n)LINKR_PROFILE_BEGIN\n([\s\S]*?)\nLINKR_PROFILE_END\n/);
  if (!match) return null;
  const source = match[1];
  const clean = source.replace(/\r/g, '').replace(/\0/g, '');
  const field = (a,b) => clean.split(a+'\n')[1]?.split(b)[0]?.trim().slice(0,1200) || '';
  return { system: clean.split('\nLINKR_OS')[0].slice(0,500), os: field('LINKR_OS','LINKR_MODEL'),
    model: field('LINKR_MODEL','LINKR_BOOT'), bootId: field('LINKR_BOOT','LINKR_DISK'),
    storage: field('LINKR_DISK','LINKR_TOOLS'), tools: [...clean.matchAll(/^TOOL:(\w+)$/gm)].map(m=>m[1]),
    observedAt: Date.now(), source: 'untrusted-target-output' };
}
