# Vendored web dependencies

- `@xterm/xterm` 6.0.0: `xterm/xterm.js`, `xterm/xterm.css`
- `@xterm/addon-fit` 0.11.0: `addon-fit/addon-fit.js`
- `marked` 18.0.12: `marked/marked.esm.js` (MIT)
- `dompurify` 3.4.15: `dompurify/purify.es.mjs` (Apache-2.0 OR MPL-2.0)

The files are copied unchanged from their npm packages so the terminal works
without internet access. Each package's license is stored beside its files.
Markdown dependencies are pinned in `mobile/package.json`; when upgrading them,
copy their ESM distributions, source maps and license files here together.
