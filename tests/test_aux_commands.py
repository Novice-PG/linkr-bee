"""Exercise the @l / @o / @r command group the Linkr Bee X1.0 exposes.

The command parser is the only thing a host touches, so it runs on the host
against stubs of the three device interfaces.
"""
import os
import re
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class AuxCommandTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-aux-")
        cls.addClassCleanup(cls.tmp.cleanup)
        directory = Path(cls.tmp.name)
        production = (ROOT / "src/linkr_aux.c").read_text()
        # Only the Zephyr includes are removed; the harness supplies the shims
        # those headers would have provided.
        production = re.sub(r"^#include <zephyr/.*\n", "", production, flags=re.M)
        (directory / "aux_production.inc").write_text(production)
        cls.binary = directory / "aux-command-test"
        subprocess.run([
            *shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall",
            "-Wextra", "-Werror", "-I", str(directory), "-I", str(ROOT / "src"),
            str(ROOT / "tests/aux_command_harness.c"), "-o", str(cls.binary),
        ], check=True)

    def test_command_parsing_and_help(self):
        result = subprocess.run([str(self.binary)], check=True,
                                capture_output=True, text=True, timeout=5)
        self.assertIn("aux command harness: ok", result.stdout)


if __name__ == "__main__":
    unittest.main()
