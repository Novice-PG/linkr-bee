/* Derive a default label for "this host". Pure so it is easy to test.
 *
 * A browser can only see its user agent, and neither native shell can read the
 * phone's own Bluetooth name, so both fall back to the OS and browser. The
 * value is only a suggestion for the label field, which the user can edit.
 *
 * The connected accessory's name is deliberately not used: it is the board's
 * advertised name, not this host's, and every phone talking to the same board
 * would then share one label. */
export function defaultHostLabel({ userAgent = "" } = {}) {
  return labelFromUserAgent(String(userAgent ?? ""));
}

export function labelFromUserAgent(ua) {
  const text = String(ua ?? "");
  if (!text) return "";
  const browser =
    /Edg\//.test(text) ? "Edge" :
    /OPR\/|Opera/.test(text) ? "Opera" :
    /Firefox\//.test(text) ? "Firefox" :
    /Chrome\//.test(text) || /CriOS/.test(text) ? "Chrome" :
    /Safari\//.test(text) ? "Safari" : "";
  const os =
    /HarmonyOS|OpenHarmony|ArkWeb/.test(text) ? "HarmonyOS" :
    /Android/.test(text) ? "Android" :
    /iPhone|iPad|iPod/.test(text) ? "iOS" :
    /Mac OS X|Macintosh/.test(text) ? "macOS" :
    /Windows/.test(text) ? "Windows" :
    /Linux/.test(text) ? "Linux" : "";
  return [browser, os].filter(Boolean).join(" · ");
}
