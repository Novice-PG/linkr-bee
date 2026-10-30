"""Exercise the release refresh: replace assets and keep the metadata honest."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
from assemble_release import FIRMWARE_BOARDS  # noqa: E402

TAG = "v0.1.2"
VERSION = "0.1.2"
APK_NAME = f"linkr-bee-{TAG}-android-debug.apk"
OTHER_NAME = f"linkr-bee-{TAG}-web.zip"


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def write_metadata(root: Path, assets: list[dict]) -> None:
    manifest = {
        "schema_version": 1,
        "project": "linkr-bee",
        "tag": TAG,
        "assets": assets,
    }
    (root / "release-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (root / "SHA256SUMS").write_text(
        "".join(f"{a['sha256']}  {a['name']}\n"
                for a in sorted(assets, key=lambda item: item["name"]))
    )


def asset(name: str, payload: bytes, kind: str, **extra) -> dict:
    return {"name": name, "size": len(payload), "sha256": digest(payload),
            "kind": kind, "build_run": 1, **extra}


def firmware_assets() -> list[dict]:
    return [
        asset(f"linkr-bee-{TAG}-firmware-{slug}.zip", b"old", "firmware",
              board=board, chip=chip)
        for slug, (board, chip) in sorted(FIRMWARE_BOARDS.items())
    ]


def command_for(root: Path, *extra: str) -> list[str]:
    return [sys.executable, str(ROOT / "tools/refresh_release.py"),
            "--tag", TAG, "--run-id", "42",
            "--manifest", str(root / "release-manifest.json"),
            "--sums", str(root / "SHA256SUMS"),
            "--out", str(root / "out"), *extra]


def write_firmware_bundles(root: Path, version: str = VERSION) -> Path:
    bundles = root / "artifacts/firmware"
    for slug in FIRMWARE_BOARDS:
        directory = bundles / f"linkr-bee-{slug}"
        directory.mkdir(parents=True)
        (directory / f"linkr-bee-{slug}.bin").write_bytes(b"image-" + slug.encode())
        (directory / "firmware.json").write_text(json.dumps({
            "project": "linkr-bee", "version": version, "board": slug,
        }))
    return bundles


class RefreshReleaseTests(unittest.TestCase):
    def test_an_apk_replacement_updates_its_entry_and_the_checksums(self):
        with tempfile.TemporaryDirectory(prefix="linkr-refresh-apk-") as tmp:
            root = Path(tmp)
            write_metadata(root, [
                asset(APK_NAME, b"old apk", "android-debug"),
                asset(OTHER_NAME, b"web", "web"),
            ])
            apk = root / "apk" / APK_NAME
            apk.parent.mkdir()
            apk.write_bytes(b"new apk")
            result = subprocess.run(command_for(root, "--apk", str(apk)),
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)

            manifest = json.loads((root / "release-manifest.json").read_text())
            entries = {a["name"]: a for a in manifest["assets"]}
            self.assertEqual(entries[APK_NAME]["size"], len(b"new apk"))
            self.assertEqual(entries[APK_NAME]["sha256"], digest(b"new apk"))
            self.assertEqual(entries[APK_NAME]["build_run"], 42)
            # The asset this run did not replace keeps its published hash.
            self.assertEqual(entries[OTHER_NAME]["sha256"], digest(b"web"))
            self.assertEqual((root / "out" / APK_NAME).read_bytes(), b"new apk")

            sums = dict(line.split("  ")[::-1]
                        for line in (root / "SHA256SUMS").read_text().splitlines())
            self.assertEqual(sums[APK_NAME], digest(b"new apk"))
            self.assertEqual(sums[OTHER_NAME], digest(b"web"))

    def test_every_firmware_board_is_rebuilt_into_its_release_shape(self):
        with tempfile.TemporaryDirectory(prefix="linkr-refresh-fw-") as tmp:
            root = Path(tmp)
            write_metadata(root, [asset(n, b"old", "firmware")
                                  for n in (f"linkr-bee-{TAG}-firmware-{s}.zip"
                                            for s in FIRMWARE_BOARDS)])
            bundles = write_firmware_bundles(root)
            result = subprocess.run(
                command_for(root, "--firmware-dir", str(bundles)),
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)

            manifest = json.loads((root / "release-manifest.json").read_text())
            entries = {a["name"]: a for a in manifest["assets"]}
            for slug in FIRMWARE_BOARDS:
                name = f"linkr-bee-{TAG}-firmware-{slug}.zip"
                bundle = root / "out" / name
                self.assertTrue(bundle.is_file(), name)
                self.assertEqual(entries[name]["sha256"],
                                 digest(bundle.read_bytes()))
                self.assertEqual(entries[name]["build_run"], 42)
                with zipfile.ZipFile(bundle) as archive:
                    names = archive.namelist()
                self.assertIn(f"linkr-bee-{slug}/firmware.json", names)
                self.assertIn(f"linkr-bee-{slug}/linkr-bee-{slug}.bin", names)

    def test_a_firmware_version_that_does_not_match_the_tag_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="linkr-refresh-bad-") as tmp:
            root = Path(tmp)
            write_metadata(root, [asset(f"linkr-bee-{TAG}-firmware-{s}.zip", b"old",
                                        "firmware") for s in FIRMWARE_BOARDS])
            bundles = write_firmware_bundles(root, version="0.1.3")
            result = subprocess.run(
                command_for(root, "--firmware-dir", str(bundles)),
                capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("reports version '0.1.3'", result.stderr)

    def test_an_asset_the_manifest_does_not_carry_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="linkr-refresh-missing-") as tmp:
            root = Path(tmp)
            write_metadata(root, [asset(OTHER_NAME, b"web", "web")])
            apk = root / "apk" / APK_NAME
            apk.parent.mkdir()
            apk.write_bytes(b"new apk")
            result = subprocess.run(command_for(root, "--apk", str(apk)),
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not carry exactly one", result.stderr)

    def test_a_manifest_for_another_release_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="linkr-refresh-tag-") as tmp:
            root = Path(tmp)
            write_metadata(root, [asset(APK_NAME, b"old apk", "android-debug")])
            manifest = json.loads((root / "release-manifest.json").read_text())
            manifest["tag"] = "v0.1.3"
            (root / "release-manifest.json").write_text(json.dumps(manifest))
            apk = root / "apk" / APK_NAME
            apk.parent.mkdir()
            apk.write_bytes(b"new apk")
            result = subprocess.run(command_for(root, "--apk", str(apk)),
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("describes 'v0.1.3'", result.stderr)

    def test_replacing_nothing_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="linkr-refresh-none-") as tmp:
            root = Path(tmp)
            write_metadata(root, [asset(APK_NAME, b"old apk", "android-debug")])
            result = subprocess.run(command_for(root),
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("nothing to replace", result.stderr)


if __name__ == "__main__":
    unittest.main()
