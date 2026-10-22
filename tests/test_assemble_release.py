"""Exercise release assembly using tiny artifact fixtures, without publishing."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]


class ReleaseAssemblyTests(unittest.TestCase):
    def test_artifacts_and_notes_point_to_the_selected_repository(self):
        for repo in (None, "https://github.example/team/linkr-bee"):
            with self.subTest(repo=repo), tempfile.TemporaryDirectory(prefix="linkr-release-test-") as tmp:
                root = Path(tmp)
                for directory in ("desktop", "firmware", "web", "apk"):
                    (root / directory).mkdir()
                targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
                           "armv7-unknown-linux-gnueabihf", "i686-unknown-linux-gnu",
                           "x86_64-pc-windows-gnu", "i686-pc-windows-gnu",
                           "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
                           "aarch64-apple-darwin", "x86_64-apple-darwin"]
                for target in targets:
                    for prefix in ("linkr-bee", "linkr-bee-cli"):
                        suffix = "zip" if "windows" in target else "tar.gz"
                        (root / "desktop" / f"{prefix}-0.1.2-{target}.{suffix}").write_bytes(b"fixture")
                        if "apple" in target:
                            (root / "desktop" / f"{prefix}-0.1.2-{target}.dmg").write_bytes(b"fixture")
                for arch in ("amd64", "arm64", "armhf", "i386"):
                    (root / "desktop" / f"linkr-bee_0.1.2_{arch}.deb").write_bytes(b"fixture")
                for board in ("esp32c3-supermini", "esp32c3-devkitm", "esp32c3-devkitc",
                              "esp32c5-devkitc", "esp32-wroom-32"):
                    bundle = root / "firmware" / f"linkr-bee-{board}"
                    bundle.mkdir()
                    (bundle / "firmware.bin").write_bytes(b"fixture")
                (root / "apk/app.apk").write_bytes(b"fixture")
                (root / "web/index.html").write_text("<title>fixture</title>")
                command = [sys.executable, str(ROOT / "tools/assemble_release.py"),
                           "--tag", "v0.1.2", "--commit", "a" * 40, "--run-id", "123",
                           "--desktop-dir", str(root / "desktop"),
                           "--firmware-dir", str(root / "firmware"),
                           "--web-dir", str(root / "web"), "--apk-dir", str(root / "apk"),
                           "--out-dir", str(root / "out"),
                           "--notes-template", str(ROOT / "docs/releases/v0.1.2.md"),
                           "--notes-out", str(root / "notes.md"),
                           "--web-version", "0.1.2", "--firmware-version", "0.1.2",
                           "--apk-version", "0.1.2"]
                if repo:
                    command.extend(["--repo", repo])
                result = subprocess.run(command, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                expected = repo or "https://github.com/radxa/linkr-bee"
                notes = (root / "notes.md").read_text()
                self.assertIn(f"{expected}/actions/runs/123", notes)
                self.assertIn(f"{expected}/blob/v0.1.2/docs/DEVELOPMENT.zh-CN.md", notes)
                self.assertNotIn("{{", notes)
                self.assertNotIn("Novice-PG/linkr-bee", notes)
                with zipfile.ZipFile(root / "out/linkr-bee-v0.1.2-web.zip") as archive:
                    readme = archive.read("linkr-bee-v0.1.2-web/README.txt").decode()
                self.assertIn(f"Source: {expected}/tree/v0.1.2", readme)
                self.assertEqual(len(list((root / "out").iterdir())), 37)
                manifest = json.loads((root / "out/release-manifest.json").read_text())
                self.assertEqual(len(manifest["assets"]), 35)
                for asset in manifest["assets"]:
                    self.assertEqual(asset["sha256"], hashlib.sha256(
                        (root / "out" / asset["name"]).read_bytes()).hexdigest())

    def test_harmony_build_code_remains_above_the_pre_unification_version(self):
        app = json.loads((ROOT / "harmonyos/AppScope/app.json5").read_text())["app"]
        # Existing installations used 1000000 before display versions unified.
        self.assertGreater(app["versionCode"], 1000000)
