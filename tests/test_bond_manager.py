"""Host tests of the real bond manager with deterministic Bluetooth/work fakes."""
import pathlib
import re
import subprocess
import tempfile
import unittest
ROOT = pathlib.Path(__file__).resolve().parents[1]
class BondManagerTests(unittest.TestCase):
    def test_list_remove_security_and_deferred_disconnect(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp)
            source = (ROOT / 'src/bond_manager.c').read_text()
            (path / 'bond_production.inc').write_text(re.sub(r'^#include[^\n]*\n', '', source, flags=re.M))
            binary = path / 'test'
            subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror', '-I', tmp,
                            str(ROOT / 'tests/bond_manager_harness.c'), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)
    def test_mutation_stays_behind_control_owner_gate(self):
        source = (ROOT / 'src/main.c').read_text()
        self.assertLess(source.index('linkr_session_command_begin(conn'), source.index('linkr_bond_command(conn'))
        readonly = source.split('bool readonly =', 1)[1].split('if (!readonly)', 1)[0]
        self.assertIn('"bonds?"', readonly)
        self.assertNotIn('bonds remove', readonly)
