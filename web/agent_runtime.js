/* Static web builds load the vendored agent runtime on demand; the Vite
 * (Capacitor) and ArkWeb builds alias this module to the source runtime in
 * mobile/src/agent-runtime.mjs, so this file only runs for plain web hosting.
 * Rebuild the bundle with tools/build_agent_bundle.sh after changing the
 * runtime or upgrading @earendil-works/*. */
export const available = true;

let runtime;

/* A dynamic import is a network fetch of about 1 MB, and it fails once in a
 * while for reasons that have nothing to do with the bundle being stale: the
 * static server dropped a connection, the tab was backgrounded mid-fetch. One
 * retry turns that from "the terminal has no assistant until the next reload"
 * into a working session. When both attempts fail the cause travels with the
 * error, because an import here can fail in ways that "rebuild the bundle"
 * would send the reader down the wrong path for. */
async function loadRuntime(attempt) {
  try {
    return await import("./vendor/agent/agent-runtime.js");
  } catch (error) {
    if (attempt > 0) {
      throw new Error(
        `The bundled assistant runtime is missing or unreadable (${error?.message || error}). Rebuild it with tools/build_agent_bundle.sh.`,
        { cause: error },
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 400));
    return loadRuntime(1);
  }
}

export async function createSerialAgent(options) {
  runtime ??= await loadRuntime(0);
  return runtime.createSerialAgent(options);
}
