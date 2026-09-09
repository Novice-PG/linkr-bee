"""Host callback tests; physical SMP, NVS persistence and radio tests remain HIL."""
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class BleSecurityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-security-")
        cls.addClassCleanup(cls.tmp.cleanup)
        directory = Path(cls.tmp.name)
        source = (ROOT / "src/ble_security.c").read_text()
        (directory / "security_production.inc").write_text(
            re.sub(r'^#include[^\n]*\n', '', source, flags=re.MULTILINE))
        cls.binary = directory / "security-test"
        subprocess.run([*shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall", "-Wextra",
                        "-Werror", "-I", str(directory), str(ROOT / "tests/ble_security_harness.c"),
                        "-o", str(cls.binary)], check=True)

    def test_gpio_gate_applies_to_new_and_replacement_keys(self):
        subprocess.run([self.binary, "gate"], check=True)

    def test_reconnect_requests_encryption_without_forcing_pairing(self):
        subprocess.run([self.binary, "reconnect"], check=True)

    def test_security_and_gpio_errors_fail_closed(self):
        subprocess.run([self.binary, "fail_closed"], check=True)

    def test_c3_build_requires_controller_encryption(self):
        command = [*shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-fsyntax-only",
                   "-DCONFIG_SOC_SERIES_ESP32C3=1", "-I", self.tmp.name,
                   str(ROOT / "tests/ble_security_harness.c")]
        disabled = subprocess.run(command, capture_output=True, text=True)
        self.assertNotEqual(disabled.returncode, 0)
        self.assertIn("pairing requires controller link encryption support", disabled.stderr)
        subprocess.run([*command, "-DCONFIG_ESP32_BT_CTLR_LE_SECURITY_ENABLE=1"], check=True)

    def test_no_plaintext_custom_gatt_attributes_or_default_nus(self):
        for name in ["ble_nus.c", "ble_uart_reliable.c", "ble_mgmt.c"]:
            source = (ROOT / "src" / name).read_text()
            for permission in re.findall(r'\bBT_GATT_PERM_\w+', source):
                self.assertIn(permission, {"BT_GATT_PERM_READ_ENCRYPT", "BT_GATT_PERM_WRITE_ENCRYPT"}, name)
        config = (ROOT / "prj.conf").read_text()
        for entry in ["CONFIG_BT_SMP=y", "CONFIG_BT_SMP_APP_PAIRING_ACCEPT=y",
                      "CONFIG_BT_BONDING_REQUIRED=y", "CONFIG_BT_SETTINGS=y",
                      "CONFIG_BT_KEYS_OVERWRITE_OLDEST=n", "CONFIG_BT_ZEPHYR_NUS_DEFAULT_INSTANCE=n"]:
            self.assertIn(entry, config.splitlines())
