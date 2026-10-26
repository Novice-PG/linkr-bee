/* Helpers shared by the native bundles (Android/iOS via Capacitor, HarmonyOS).
 *
 * The flasher is a browser feature: it needs Web Serial, which the native
 * WebViews do not expose. Keep it out of those packages instead of shipping a
 * link that opens a missing page and libraries nothing can call. The plain web
 * build still contains the whole flasher.
 */
import fs from "node:fs";
import path from "node:path";

export const WEB_ONLY_VENDOR = new Set(["esptool-js", "jszip"]);

export function stripWebFlasher(html) {
  return html.replace(
    /\s*<!-- web-flasher:start -->[\s\S]*?<!-- web-flasher:end -->\s*/g,
    "\n",
  );
}

export function copyNativeVendor(sourceDir, destinationDir) {
  fs.cpSync(sourceDir, destinationDir, {
    recursive: true,
    filter(source) {
      const relative = path.relative(sourceDir, source);
      if (!relative) return true;
      const [top] = relative.split(path.sep);
      return !WEB_ONLY_VENDOR.has(top);
    },
  });
}
