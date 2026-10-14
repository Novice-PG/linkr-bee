#!/usr/bin/env sh
# Build and package the host terminal for one target.
#
# One definition, two callers: the CI matrix and a developer's shell. The layout
# of an archive is part of the promise a release makes, so it is written here
# rather than in the workflow *and* in somebody's shell history.
#
# Usage:
#   dev/package.sh <target> [outdir]
#   dev/package.sh x86_64-unknown-linux-gnu
#   dev/package.sh x86_64-pc-windows-gnu dist
#
# Produces <outdir>/linkr-<version>-<target>.tar.gz holding
#   linkr-<version>-<target>/linkr[.exe]   the binary
#   linkr-<version>-<target>/README.md     what it is and how to run it
#   linkr-<version>-<target>/LICENSE       the licence it ships under
#
# `tar.gz` for every platform including Windows: `tar.exe` has shipped with
# Windows since 10 1803, and one archive format is one thing to verify.
set -eu

target=${1:?usage: package.sh <target> [outdir]}
outdir=${2:-dist}

crate_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
repo_dir=$(CDPATH='' cd -- "$crate_dir/../.." && pwd)
version=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' "$crate_dir/Cargo.toml" | head -1)
if [ -z "$version" ]; then
  echo "error: no version in $crate_dir/Cargo.toml" >&2
  exit 1
fi

exe=
case "$target" in
  *windows*) exe=.exe ;;
esac

name="linkr-$version-$target"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# `--locked` because Cargo.lock is committed: a release must be the dependency
# set that was tested, not whatever the registry had that morning.
cargo build --release --locked --target "$target" --manifest-path "$crate_dir/Cargo.toml"

mkdir -p "$stage/$name"
cp "$crate_dir/target/$target/release/linkr$exe" "$stage/$name/"
cp "$crate_dir/README.md" "$stage/$name/README.md"
if [ -f "$repo_dir/LICENSE" ]; then
  cp "$repo_dir/LICENSE" "$stage/$name/LICENSE"
fi

mkdir -p "$outdir"
archive=$(CDPATH='' cd -- "$outdir" && pwd)/"$name.tar.gz"
(umask 022 && cd "$stage" && tar -czf "$archive" "$name")

echo "$archive"
