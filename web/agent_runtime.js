/* Static web builds load the vendored agent runtime on demand; the Vite
 * (Capacitor) and ArkWeb builds alias this module to the source runtime in
 * mobile/src/agent-runtime.mjs, so this file only runs for plain web hosting.
 * Rebuild the bundle with tools/build_agent_bundle.sh after changing the
 * runtime or upgrading @earendil-works/*. */
export const available = true;

let runtime;
export async function createSerialAgent(options) {
  if (!runtime) {
    try {
      runtime = await import("./vendor/agent/agent-runtime.js");
    } catch (error) {
      /* The cause has to travel with the message: an import here can fail for
       * reasons that have nothing to do with the bundle being stale -- a syntactically
       * valid but unloadable module, a server answering with the wrong type, a
       * denied fetch -- and "rebuild the bundle" sends the reader down the wrong
       * path for all of them. */
      throw new Error(
        `The bundled assistant runtime is missing or unreadable (${error?.message || error}). Rebuild it with tools/build_agent_bundle.sh.`,
        { cause: error },
      );
    }
  }
  return runtime.createSerialAgent(options);
}
