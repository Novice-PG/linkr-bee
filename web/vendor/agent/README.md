# Vendored agent runtime

`agent-runtime.js` is the assistant runtime for the plain web build
(`tools/serve_web.sh`), which has no bundler. It is an esbuild bundle of
`mobile/src/agent-runtime.mjs`, so the terminal can offer the assistant without
npm, a build step, or internet access.

Do not edit the generated files by hand:

```sh
tools/build_agent_bundle.sh
```

That command rewrites all three files below. Run it after changing
`mobile/src/agent-runtime.mjs`, `mobile/src/pi-agent.mjs`, any `web/` module they
import, or after upgrading the packages in `mobile/package.json`.

- `agent-runtime.js`: ESM bundle, browser target, minified, license comments
  kept at the end of the file.
- `LICENSES.txt`: attribution for every bundled package.
- `BUILD.json`: entry, esbuild version, bundled package versions and the bundle
  digest, so a stale artifact is detectable (`mobile/test/agent_bundle.test.mjs`).

The Vite (Capacitor) and ArkWeb builds do not use this bundle: they alias
`web/agent_runtime.js` to the source runtime and bundle it themselves.

## Notes

- The bundle is fetched only when a question is asked, so the terminal keeps its
  original load time.
- Browser requests to the model endpoint are subject to CORS. Local model servers
  (Ollama, LM Studio, llama.cpp) and gateways that allow browser origins work
  directly; most hosted providers do not send CORS headers and need a proxy.
