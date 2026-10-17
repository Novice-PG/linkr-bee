#!/usr/bin/env sh
# Read one release archive back and say whether it is the archive it claims to
# be: a folder named after the archive holding the binary and the two documents
# it ships with, and nothing else.
#
# Usage:
#   dev/check-archive.sh <archive>
#   dev/check-archive.sh dist/*
#
# One definition, every caller: all ten packaging legs run this over their own
# `dist`, whichever runner they are on, so no leg can pass by being checked less
# than the others. The listing tool is the platform's own — `unzip` where it
# exists, bsdtar (`tar`) where it does not, `dpkg-deb` for a package — and a
# format the host cannot read is reported as skipped rather than passed, which
# is why a `.dmg` is left to the smoke step that mounts it.
set -eu

archive=${1:?usage: check-archive.sh <archive>}
[ -f "$archive" ] || {
  echo "error: no $archive" >&2
  exit 1
}
stem=$(basename "$archive")

case "$stem" in
  *.deb)
    # `dpkg-deb -c` prints a long listing, and the obvious `| grep -q` would
    # end the reader the moment it matched, killing the writer with SIGPIPE.
    # Capture first, match on the string.
    fields=$(dpkg-deb -f "$archive" Package Architecture)
    echo "$fields"
    printf '%s\n' "$fields" | grep -qx 'Package: linkr' || {
      echo "unexpected package name" >&2
      exit 1
    }
    contents=$(dpkg-deb -c "$archive")
    printf '%s\n' "$contents" | grep -E ' \./usr/bin/linkr$' >/dev/null || {
      echo "the package holds no binary" >&2
      exit 1
    }
    echo "$archive: ok (deb)"
    exit 0
    ;;
  *.tar.gz) listing=$(tar -tzf "$archive") ;;
  *.zip)
    # GNU tar cannot read a zip and bsdtar can, which is the whole of the
    # platform difference here: ask `unzip` first and fall back to `tar`.
    if unzip -Z1 "$archive" >/dev/null 2>&1; then
      listing=$(unzip -Z1 "$archive")
    else
      listing=$(tar -tf "$archive")
    fi
    ;;
  *.dmg)
    if [ "$(uname -s)" = Darwin ]; then
      echo "$archive: a volume image — proved by mounting it, which the smoke step does where this target can run"
    else
      echo "$archive: a volume image — skipped, only macOS can mount it"
    fi
    exit 0
    ;;
  *)
    echo "$archive: not a format this checks"
    exit 0
    ;;
esac

echo "$listing"
name=${stem%.tar.gz}
name=${name%.zip}
bin=linkr
case "$name" in linkr-cli-*) bin=linkr-cli ;; esac
# Nothing may be in here that the script did not put there: a stray file in a
# release is uploaded before anyone reads the diff.
allowed=$(printf '%s\n' "$name/" "$name/$bin" "$name/$bin.exe" \
                        "$name/README.md" "$name/LICENSE")
for entry in $listing; do
  printf '%s\n' "$allowed" | grep -qx "$entry" || {
    echo "unexpected entry in the archive: $entry" >&2
    exit 1
  }
done
# And one of them must be the binary itself.
printf '%s\n' "$listing" | grep -E "^$name/$bin(\.exe)?$" >/dev/null || {
  echo "the archive holds no binary" >&2
  exit 1
}
echo "$archive: ok"
