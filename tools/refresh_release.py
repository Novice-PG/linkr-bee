#!/usr/bin/env python3
"""Replace some assets of an existing release and keep its metadata honest.

A release is a snapshot of the tagged commit, but the components move on: the
APK wraps the Web terminal, and the firmware images are built from the same
tree. Rather than publish a new version for every rebuild, this script takes the
files a fresh build produced, rebuilds them into the asset shapes the release
already carries, and updates ``release-manifest.json`` and ``SHA256SUMS`` so the
published hashes still describe the files beside them.

Only the assets named on the command line are replaced; everything else in the
release keeps the bytes it was published with.

Usage:
    tools/refresh_release.py \
        --tag v0.1.0 --run-id 123 \
        --manifest meta/release-manifest.json \
        --sums meta/SHA256SUMS \
        --out release-out \
        --apk apk/linkr-bee-v0.1.0-android-debug.apk \
        --firmware-dir artifacts/firmware

The firmware directory holds one directory per board, named
``linkr-bee-<slug>``, exactly as the workflow's upload step names the artifact.
"""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path

from assemble_release import FIRMWARE_BOARDS, sha256, zip_tree


def fail(message: str) -> None:
    """Stop with a message the workflow log shows verbatim."""
    raise SystemExit(message)


def patch_entry(manifest: dict, name: str, path: Path, run_id: int | None) -> None:
    """Point one manifest asset at the rebuilt file, refusing an unknown name."""
    matches = [asset for asset in manifest["assets"] if asset["name"] == name]
    if len(matches) != 1:
        fail(
            f"the manifest does not carry exactly one {name} entry "
            f"(found {len(matches)}); refusing to publish a hash nobody else has"
        )
    entry = matches[0]
    entry["size"] = path.stat().st_size
    entry["sha256"] = sha256(path)
    if run_id is not None:
        entry["build_run"] = run_id
    print(f"replaced {name}: {entry['size']} bytes, {entry['sha256']}")


def rebuild_firmware(source: Path, out: Path, tag: str) -> list[Path]:
    """Zip each board's artifact directory into the release's firmware shape."""
    tag_version = tag.removeprefix("v")
    bundles: list[Path] = []
    for slug in sorted(FIRMWARE_BOARDS):
        directory = source / f"linkr-bee-{slug}"
        if not directory.is_dir():
            fail(f"no firmware artifact for {slug} at {directory}")
        metadata = directory / "firmware.json"
        if not metadata.is_file():
            fail(f"firmware artifact {slug} has no firmware.json")
        data = json.loads(metadata.read_text(encoding="utf-8"))
        if data.get("version") != tag_version:
            fail(
                f"firmware {slug} reports version {data.get('version')!r}, "
                f"expected {tag_version!r}"
            )
        destination = out / f"linkr-bee-{tag}-firmware-{slug}.zip"
        zip_tree(directory, destination, f"linkr-bee-{slug}")
        bundles.append(destination)
    return bundles


def write_metadata(manifest_path: Path, sums_path: Path, manifest: dict) -> None:
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    # The release's SHA256SUMS names every asset in sorted order, the same order
    # tools/assemble_release.py writes it in.
    sums_path.write_text(
        "".join(
            f"{asset['sha256']}  {asset['name']}\n"
            for asset in sorted(manifest["assets"], key=lambda item: item["name"])
        ),
        encoding="utf-8",
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", required=True)
    parser.add_argument("--run-id", type=int)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--sums", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--apk", type=Path)
    parser.add_argument("--firmware-dir", type=Path)
    args = parser.parse_args()

    if args.apk is None and args.firmware_dir is None:
        fail("nothing to replace: pass --apk, --firmware-dir or both")

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    if manifest.get("tag") != args.tag:
        fail(
            f"the manifest describes {manifest.get('tag')!r}, "
            f"not the release {args.tag!r}"
        )

    args.out.mkdir(parents=True, exist_ok=True)

    if args.apk is not None:
        if not args.apk.is_file():
            fail(f"no APK at {args.apk}")
        destination = args.out / args.apk.name
        shutil.copy2(args.apk, destination)
        patch_entry(manifest, destination.name, destination, args.run_id)

    if args.firmware_dir is not None:
        for bundle in rebuild_firmware(args.firmware_dir, args.out, args.tag):
            patch_entry(manifest, bundle.name, bundle, args.run_id)

    write_metadata(args.manifest, args.sums, manifest)
    print(f"metadata written to {args.manifest} and {args.sums}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
