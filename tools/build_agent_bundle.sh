#!/usr/bin/env sh
# Build the vendored agent runtime for the static web terminal.
#
# The plain web build (tools/serve_web.sh) has no bundler, so the assistant is
# shipped as a prebuilt ESM bundle beside the other vendored dependencies. The
# Vite/Capacitor and ArkWeb builds alias web/agent_runtime.js to the source
# runtime and do not use this bundle.
#
# Usage: tools/build_agent_bundle.sh
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
entry="$repo_dir/mobile/src/agent-runtime.mjs"
out_dir="$repo_dir/web/vendor/agent"
esbuild="$repo_dir/mobile/node_modules/.bin/esbuild"

if [ ! -x "$esbuild" ]; then
    echo "esbuild not found: run 'npm ci' in mobile/ first" >&2
    exit 1
fi

[ -f "$entry" ] || { echo "missing entry: $entry" >&2; exit 1; }
mkdir -p "$out_dir"

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

# Deterministic flags: the same sources and esbuild version must produce a
# byte-identical bundle so tools/verify.sh and the unit tests can detect a
# stale committed artifact.
"$esbuild" "$entry" \
    --bundle --format=esm --platform=browser --target=es2022 \
    --minify --legal-comments=eof \
    --metafile="$work_dir/meta.json" \
    --outfile="$out_dir/agent-runtime.js"

# Attribution: list every bundled package with its version and license, and
# concatenate the license texts that ship with them.
node - "$work_dir/meta.json" "$repo_dir" "$out_dir" <<'EOF'
const fs = require("fs");
const path = require("path");
const [metaPath, repoDir, outDir] = process.argv.slice(2);
const meta = JSON.parse(fs.readFileSync(metaPath, "utf8"));

const names = new Set();
for (const input of Object.keys(meta.inputs)) {
  const match = input.match(/node_modules\/((?:@[^/]+\/)?[^/]+)\//);
  if (match) names.add(match[1]);
}

const packages = {};
const sections = [];
for (const name of [...names].sort()) {
  const dir = path.join(repoDir, "mobile/node_modules", name);
  const manifest = JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  packages[name] = manifest.version;
  const licenseFile = ["LICENSE", "LICENSE.md", "LICENSE.txt", "COPYING"]
    .map((file) => path.join(dir, file))
    .find((file) => fs.existsSync(file));
  sections.push(licenseFile
    ? `===== ${name} ${manifest.version} (${manifest.license || "see text"}) =====\n\n` +
      fs.readFileSync(licenseFile, "utf8").trim()
    : `===== ${name} ${manifest.version} (${manifest.license || "unknown"}) =====\n\n` +
      "This package declares the license above but ships no license file.");
}

const bundle = path.join(outDir, "agent-runtime.js");
const build = {
  entry: "mobile/src/agent-runtime.mjs",
  esbuild: require(path.join(repoDir, "mobile/node_modules/esbuild/package.json")).version,
  packages,
  sha256: require("crypto").createHash("sha256").update(fs.readFileSync(bundle)).digest("hex"),
};

fs.writeFileSync(path.join(outDir, "LICENSES.txt"), sections.join("\n\n") + "\n");
fs.writeFileSync(path.join(outDir, "BUILD.json"), JSON.stringify(build, null, 2) + "\n");
console.log(`bundled ${Object.keys(packages).length} packages from ${meta.inputs ? Object.keys(meta.inputs).length : 0} inputs`);
EOF

ls -l "$out_dir/agent-runtime.js" >&2
echo "agent runtime bundle written to web/vendor/agent/" >&2
