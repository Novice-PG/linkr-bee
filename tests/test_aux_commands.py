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
        main = (ROOT / "src/main.c").read_text()
        defines = re.search(r"^#define CONTROL_CMD_MAX_LEN .*", main, re.M).group(0)
        defines += "\n" + re.search(r"^#define LINKR_MGMT_MAX_PAYLOAD .*", (ROOT / "src/ble_mgmt.h").read_text(), re.M).group(0)
        start = main.index('\tif (len >= sizeof(control_command_data))')
        end = main.index('\n\tis_long =', start)
        (directory / "control_limit.inc").write_text(defines + """
static char control_command_data[CONTROL_CMD_MAX_LEN];
static int send_control_response(const void *conn, const char *format, ...) {
 (void)conn; (void)format; return 0;
}
static bool command_rejected(uint16_t len) {
 const void *conn = NULL;
""" + main[start:end] + "\n return false;\n}\n")
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
