"""Compile the actual ownership arbiter with recursive host mutexes."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class SerialSessionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-session-")
        cls.addClassCleanup(cls.tmp.cleanup)
        path = Path(cls.tmp.name)
        (path / "zephyr/sys").mkdir(parents=True)
        (path / "zephyr/kernel.h").write_text(r'''
#include <pthread.h>
#define K_FOREVER -1
struct k_mutex { pthread_mutex_t native; };
#define K_MUTEX_DEFINE(name) \
struct k_mutex name; \
__attribute__((constructor)) static void init_##name(void) { \
 pthread_mutexattr_t attr; pthread_mutexattr_init(&attr); \
 pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE); \
 pthread_mutex_init(&name.native, &attr); pthread_mutexattr_destroy(&attr); }
static inline void k_mutex_lock(struct k_mutex *m, int timeout) {
 (void)timeout; pthread_mutex_lock(&m->native);
}
static inline void k_mutex_unlock(struct k_mutex *m) { pthread_mutex_unlock(&m->native); }
''')
        (path / "zephyr/sys/printk.h").write_text("#include <stdio.h>\n#define snprintk snprintf\n")
        cls.binary = path / "session-test"
        subprocess.run([
            *shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-D_GNU_SOURCE",
            "-Wall", "-Wextra", "-Werror", "-pthread", "-DCONFIG_BT_MAX_CONN=2",
            "-I", str(path), "-I", str(ROOT / "src"), str(ROOT / "src/serial_session.c"),
            str(ROOT / "tests/serial_session_harness.c"), "-o", str(cls.binary),
        ], check=True)

    def test_real_arbiter_scenarios(self):
        for scenario in ["single_writer", "managed_release", "handover", "reconnect",
                         "queued_command", "concurrent_claims", "disconnected",
                         "bad_handover", "legacy_lan"]:
            with self.subTest(scenario=scenario):
                result = subprocess.run([str(self.binary), scenario], capture_output=True,
                                        text=True, check=True, timeout=10)
                self.assertEqual(result.stdout.strip(), "ok")


if __name__ == "__main__":
    unittest.main()
