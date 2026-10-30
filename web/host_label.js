/* Derive a default label for "this host". Pure so it is easy to test.
 *
 * The phone apps run inside a native shell and can read the connected
 * accessory's device name; a plain browser cannot see anything but its user
 * agent, so it falls back to the OS and browser. Either way the value is only
 * a suggestion for the label field, which the user can edit. */
export function defaultHostLabel({ userAgent = "", deviceName = "", platform = "web" } = {}) {
  const name = String(deviceName ?? "").trim();
  if (platform !== "web" && name) return name;
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
