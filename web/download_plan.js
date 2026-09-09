export const DOWNLOAD_PROBE = `for t in curl wget sha256sum shasum openssl; do command -v "$t" >/dev/null 2>&1 && printf 'LINKR_TOOL:%s\\n' "$t"; done; :`;
const quote = value => "'" + value.replaceAll("'", "'\\''") + "'";
export function validateDownload({ url, sha256 = "" }) {
  const parsed = new URL(url);
  if (!["https:", "http:"].includes(parsed.protocol) || parsed.username || parsed.password) throw new Error("Download requires an HTTP(S) URL without credentials.");
  if (sha256 && !/^[a-f0-9]{64}$/i.test(sha256)) throw new Error("Expected SHA-256 must contain 64 hexadecimal characters.");
  return { url: parsed.href, sha256: sha256.toLowerCase() };
}
export function targetDownloadPlan(args, probe) {
  const { url, sha256 } = validateDownload(args);
  if (!probe || probe.executionStatus !== "completed" || probe.exitCode !== 0 || probe.evidenceTruncated) throw new Error("Complete probe_download_tools and inspect its result before downloading.");
  const available = [...probe.evidence.matchAll(/^LINKR_TOOL:(curl|wget|sha256sum|shasum|openssl)\r?$/gm)].map(m => m[1]);
  const downloader = available.includes("curl") ? "curl" : available.includes("wget") ? "wget" : null;
  const checksum = ["sha256sum", "shasum", "openssl"].find(t => available.includes(t));
  if (!downloader || !checksum) throw new Error("Target needs curl/wget and a SHA-256 tool. Report missing tools before choosing another action.");
  const path = args.path;
  if (typeof path !== "string" || !path.startsWith("/") || path.endsWith("/") || /[\x00-\x1f\x7f]/.test(path)) throw new Error("Target destination must be an absolute file path without control characters.");
  const fetch = downloader === "curl" ? 'curl -fL --progress-bar -o "$p" ' : 'wget -O "$p" ';
  const hash = checksum === "sha256sum" ? 'sha256sum "$p"' : checksum === "shasum" ? 'shasum -a 256 "$p"' : 'openssl dgst -sha256 "$p"';
  const command = `d=${quote(path)}; test ! -e "$d" || exit 73; p=$(mktemp "$d.part.XXXXXX") || exit; printf 'LINKR_PART:%s\\n' "$p"; ${fetch}${quote(url)} || exit; h=$(${hash}) || exit; h=\${h${checksum === "openssl" ? "##* " : "%% *"}}; printf '\\nLINKR_SHA256:%s\\n' "$h"; ${sha256 ? `test "$h" = ${quote(sha256)} || exit 65; ` : ""}ln "$p" "$d" || exit; rm "$p"; printf 'LINKR_BYTES:'; wc -c < "$d"`;
  return { command, metadata: { destination: "target", path, url, downloader, checksum, expectedSha256: sha256 || null } };
}
