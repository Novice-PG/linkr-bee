"""Exercise production fault recovery and per-owner heartbeat bookkeeping."""
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class WatchdogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-watchdog-")
        cls.addClassCleanup(cls.tmp.cleanup)
        directory = Path(cls.tmp.name)
        production = (ROOT / "src/watchdog.c").read_text()
        # Only the Zephyr includes are replaced; all policy functions compile
        # directly from the production file against the host fakes.
        production = re.sub(r"^#include <zephyr/.*\n", "", production, flags=re.M)
        (directory / "watchdog_production.inc").write_text(production)
        cls.binary = directory / "watchdog-test"
        subprocess.run([
            *shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall",
            "-Wextra", "-Werror", "-I", str(directory), "-I", str(ROOT / "src"),
            str(ROOT / "tests/watchdog_harness.c"), "-o", str(cls.binary),
        ], check=True)

    def scenario(self, name):
        subprocess.run([str(self.binary), name], check=True, capture_output=True, timeout=5)

    def test_feeds_are_throttled_and_cannot_cover_another_worker(self):
        self.scenario("feeds")

    def test_expired_worker_reboots_and_identifies_its_owner(self):
        self.scenario("expired")

    def test_fatal_handler_flushes_and_reboots(self):
        self.scenario("fatal")

    def test_unavailable_hardware_is_not_silently_ignored(self):
        self.scenario("device-failure")

    def test_failed_driver_initialization_reboots(self):
        self.scenario("init-failure")

    def test_failed_channel_registration_reboots(self):
        self.scenario("channel-failure")
