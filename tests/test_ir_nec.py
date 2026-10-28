"""Exercise the NEC codec that the IR interrupt handler runs.

src/ir_nec.c has no Zephyr dependencies on purpose: the mark/space classifier
is where remote-control bugs live, and it can be driven from the host.
"""
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class IrNecTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-ir-nec-")
        cls.addClassCleanup(cls.tmp.cleanup)
        cls.binary = Path(cls.tmp.name) / "ir-nec-test"
        subprocess.run([
            *shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall",
            "-Wextra", "-Werror", "-I", str(ROOT / "src"),
            str(ROOT / "tests/ir_nec_harness.c"),
            str(ROOT / "src/ir_nec.c"), "-o", str(cls.binary),
        ], check=True)

    def test_encode_decode_roundtrip_and_framing(self):
        result = subprocess.run([str(self.binary)], check=True,
                                capture_output=True, text=True, timeout=5)
        self.assertIn("ir_nec harness: ok", result.stdout)


if __name__ == "__main__":
    unittest.main()
