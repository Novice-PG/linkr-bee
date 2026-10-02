#!/usr/bin/env sh
# Release build of the Rust terminal (tools/linkr-cli) for the host, or for an
# explicit --target triple. The Windows equivalent is tools/build_terminal.ps1;
# the self-extracting Windows bundle comes from tools/build_terminal_bundle.py
# (or `powershell -File tools\build_terminal.ps1 -Bundle`).
#
# Usage: tools/build_terminal.sh [--target TRIPLE] [--debug] [--no-verify]
#   --target TRIPLE  cross-build with `cargo build --target TRIPLE`; the triple's
#                    std must be installed (`rustup target add TRIPLE`) and its
#                    system libraries (libdbus-1 on Linux) must be findable
#   --debug          build the dev profile instead of release
#   --no-verify      skip `cargo fmt --check`, clippy and the test suite
#
# The binary, its SHA256SUMS and the platform slug end up in
# dist/linkr-terminal-<slug>/.
set -eu

repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cli_dir="$repo_dir/tools/linkr-cli"
target=""
profile="release"
verify=1

usage() {
    # The header comment block doubles as the help text.
    sed -n '2,$s/^# \{0,1\}//p' "$0"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            [ $# -ge 2 ] || { echo "build_terminal.sh: --target needs a triple" >&2; exit 2; }
            target=$2
            shift 2
            ;;
        --target=*)
            target=${1#*=}
            shift
            ;;
        --debug)
            profile="debug"
            shift
            ;;
        --no-verify)
            verify=0
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "build_terminal.sh: unknown argument: $1" >&2
            echo "Usage: tools/build_terminal.sh [--target TRIPLE] [--debug] [--no-verify]" >&2
            exit 2
            ;;
    esac
done

command -v cargo >/dev/null 2>&1 || {
    echo "build_terminal.sh: cargo not found; install Rust from https://rustup.rs" >&2
    exit 1
}

if [ "$verify" -eq 1 ]; then
    echo "==> cargo fmt --check"
    (cd "$cli_dir" && cargo fmt --check)
    echo "==> cargo clippy --all-targets"
    (cd "$cli_dir" && cargo clippy --all-targets -- -D warnings)
    echo "==> cargo test"
    (cd "$cli_dir" && cargo test)
fi

set -- build "--$profile"
[ -n "$target" ] && set -- "$@" --target "$target"
echo "==> cargo $*"
(cd "$cli_dir" && cargo "$@")

if [ -n "$target" ]; then
    target_dir="$cli_dir/target/$target"
    slug=$target
else
    target_dir="$cli_dir/target"
    slug="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
fi

binary="$target_dir/$profile/linkr"
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) binary="$target_dir/$profile/linkr.exe" ;;
esac
[ -f "$binary" ] || {
    echo "build_terminal.sh: build finished without $binary" >&2
    exit 1
}

out_dir="$repo_dir/dist/linkr-terminal-$slug"
mkdir -p "$out_dir"
cp "$binary" "$out_dir/"

sums="$out_dir/SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$out_dir" && sha256sum "$(basename "$binary")" >SHA256SUMS)
elif command -v shasum >/dev/null 2>&1; then
    (cd "$out_dir" && shasum -a 256 "$(basename "$binary")" >SHA256SUMS)
else
    sums=""
fi

version=$("$out_dir/$(basename "$binary")" --version 2>/dev/null || true)
echo "Built: $out_dir/$(basename "$binary")${version:+ ($version)}${sums:+ + SHA256SUMS}"
