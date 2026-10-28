"""Exercise the IR learning capture and its persistence format.

src/ir_learn.c holds the capture rules and the NVS blob layout; both are pure
logic so a host can drive them without an IR receiver or a flash partition.
"""
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class IrLearnTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-ir-learn-")
        cls.addClassCleanup(cls.tmp.cleanup)
        cls.binary = Path(cls.tmp.name) / "ir-learn-test"
        subprocess.run([
            *shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall",
            "-Wextra", "-Werror", "-I", str(ROOT / "src"),
            str(ROOT / "tests/ir_learn_harness.c"),
            str(ROOT / "src/ir_learn.c"), str(ROOT / "src/ir_nec.c"),
            "-o", str(cls.binary),
        ], check=True)

    def test_capture_replay_and_storage_blob(self):
        result = subprocess.run([str(self.binary)], check=True,
                                capture_output=True, text=True, timeout=5)
        self.assertIn("ir_learn harness: ok", result.stdout)


if __name__ == "__main__":
    unittest.main()
