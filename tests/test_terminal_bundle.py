"""Unit tests for the self-extracting Windows bundle generator.

Run with: python3 -m unittest discover -s tests -v

The generator (tools/build_terminal_bundle.py) turns a Windows `linkr.exe`
into a single `linkr-bee-terminal.ps1` that unpacks itself and runs the
terminal. These tests pin the parts a user depends on: the payload survives a
round trip, the checksum in the script matches the payload, the script runs the
terminal in the same console and forwards its exit code, and the command line
rejects a payload it cannot name safely.
"""

import base64
import hashlib
import importlib.util
import io
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
GENERATOR_PATH = ROOT / "tools" / "build_terminal_bundle.py"


def load_generator():
    spec = importlib.util.spec_from_file_location("linkr_terminal_bundle", GENERATOR_PATH)
    module = importlib.util.module_from_spec(spec)
    with patch.dict("sys.modules", {spec.name: module}):
        spec.loader.exec_module(module)
    return module


generator = load_generator()

# Bigger than one base64 line, and holds every byte value, so the round trip
# exercises chunking as well as the encoding itself.
PAYLOAD = bytes(range(256)) * 40


def embedded_payload(script: str) -> bytes:
    """Pull the base64 block back out of a generated script."""
    start = script.index("$payload = @'", script.index(generator.PAYLOAD_BEGIN))
    body = script[start + len("$payload = @'"):]
    body = body[: body.index("'@")]
    return base64.b64decode(body)


def build_script(**overrides) -> str:
    options = {"name": "linkr.exe", "version": "9.9.9", "source": "linkr.exe"}
    options.update(overrides)
    return generator.build_script(PAYLOAD, **options)


class BundleScriptTests(unittest.TestCase):
    def test_payload_round_trips(self):
        self.assertEqual(embedded_payload(build_script()), PAYLOAD)

    def test_payload_lines_are_short_and_plain_base64(self):
        begin = build_script().index(generator.PAYLOAD_BEGIN)
        end = build_script().index(generator.PAYLOAD_END)
        lines = build_script()[begin:end].splitlines()[2:-1]
        self.assertGreater(len(lines), 1)
        for line in lines:
            self.assertLessEqual(len(line), generator.B64_LINE)
            self.assertRegex(line, r"^[A-Za-z0-9+/]*={0,2}$")

    def test_embedded_checksum_matches_the_payload(self):
        script = build_script()
        digest = re.search(r"\$payloadSha256 = '([0-9a-f]{64})'", script)
        self.assertIsNotNone(digest, "a 64-hex checksum must be stamped in")
        self.assertEqual(digest.group(1), hashlib.sha256(PAYLOAD).hexdigest())

    def test_script_unpacks_once_and_runs_the_terminal(self):
        script = build_script()
        # Unpack path and verification before anything executes.
        self.assertIn("$ErrorActionPreference = 'Stop'", script)
        self.assertIn("Join-Path $env:LOCALAPPDATA 'LinkrBee\\bin'", script)
        self.assertIn("Get-FileHash -LiteralPath $path -Algorithm SHA256", script)
        self.assertIn("[Convert]::FromBase64String", script)
        self.assertIn("[IO.File]::WriteAllBytes($exePath, $bytes)", script)
        # The terminal runs in this console with the script's own arguments,
        # and its exit code leaves the script untouched.
        self.assertIn("-Wait -PassThru -NoNewWindow", script)
        self.assertIn("foreach ($argument in $args)", script)
        self.assertIn("exit $process.ExitCode", script)
        self.assertNotIn("Start-Process powershell", script)

    def test_payload_name_and_version_are_stamped_in(self):
        script = build_script(name="linkr.exe", version="9.9.9")
        self.assertIn("$payloadName = 'linkr.exe'", script)
        self.assertIn("$payloadVersion = '9.9.9'", script)
        self.assertIn("tools/build_terminal_bundle.py", script)

    def test_header_documents_the_usual_invocations(self):
        script = build_script()
        header = script[: script.index("Set-StrictMode")]
        self.assertIn("powershell -ExecutionPolicy Bypass -File", header)
        self.assertIn("--scan", header)
        self.assertIn("--tui", header)
        self.assertIn("%LOCALAPPDATA%", header)

    def test_unsafe_inputs_are_refused(self):
        with self.assertRaises(ValueError):
            generator.build_script(b"", name="linkr.exe", version="1.0.0")
        with self.assertRaises(ValueError):
            generator.build_script(PAYLOAD, name="../evil.exe", version="1.0.0")
        with self.assertRaises(ValueError):
            generator.build_script(PAYLOAD, name="linkr.exe", version="1.0.0'; Start-Process calc")


class BundleCommandTests(unittest.TestCase):
    def generate(self, *extra) -> tuple[int, bytes, str]:
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / "linkr.exe"
            exe.write_bytes(PAYLOAD)
            out = Path(tmp) / "linkr-bee-terminal.ps1"
            with patch("sys.stdout", new=io.StringIO()) as stdout:
                code = generator.main(
                    ["--exe", str(exe), "--output", str(out), *extra]
                )
            return code, out.read_bytes(), stdout.getvalue()

    def test_writes_a_crlf_script_with_a_round_trippable_payload(self):
        code, data, printed = self.generate()
        self.assertEqual(code, 0)
        self.assertIn("Bundle:", printed)
        self.assertIn(b"\r\n", data)
        self.assertNotIn(b"\n", data.replace(b"\r\n", b""))
        script = data.decode("utf-8")
        self.assertEqual(embedded_payload(script), PAYLOAD)
        self.assertIn("linkr-bee-terminal.ps1", script)

    def test_version_defaults_to_the_cargo_version(self):
        _, data, _ = self.generate()
        cargo = re.search(
            r'^version\s*=\s*"([^"]+)"',
            (ROOT / "tools" / "linkr-cli" / "Cargo.toml").read_text("utf-8"),
            re.M,
        )
        self.assertIsNotNone(cargo)
        self.assertIn(f"$payloadVersion = '{cargo.group(1)}'", data.decode("utf-8"))

    def test_stdout_prints_instead_of_writing(self):
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / "linkr.exe"
            exe.write_bytes(PAYLOAD)
            out = Path(tmp) / "bundle.ps1"
            with patch("sys.stdout", new=io.StringIO()) as stdout:
                code = generator.main(
                    ["--exe", str(exe), "--output", str(out), "--stdout"]
                )
            self.assertEqual(code, 0)
            self.assertFalse(out.exists())
            self.assertIn("$payload = @'", stdout.getvalue())

    def test_missing_executable_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch("sys.stderr", new=io.StringIO()) as stderr:
                code = generator.main(
                    ["--exe", str(Path(tmp) / "absent.exe"), "--stdout"]
                )
            self.assertEqual(code, 1)
            self.assertIn("no such executable", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
