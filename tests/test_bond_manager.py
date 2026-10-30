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
        readonly = source.split('bool readonly =', 1)[1].split('bool own_bond', 1)[0]
        # Comments above this block name the commands too; only code counts.
        readonly = re.sub(r'/\*.*?\*/', '', readonly, flags=re.S)
        self.assertIn('"bonds?"', readonly)
        self.assertIn('"bonds names?"', readonly)
        self.assertNotIn('bonds remove', readonly)
        self.assertNotIn('bonds name ', readonly)

    def test_label_write_is_exempt_from_console_ownership(self):
        """bonds name only writes the caller's own bond, so it must not need a
        claim while the shared mutations still do."""
        source = (ROOT / 'src/main.c').read_text()
        self.assertIn('if (!readonly && !own_bond)', source)
        own = source.split('bool own_bond =', 1)[1].split(';', 1)[0]
        self.assertIn('"bonds name"', own)
        self.assertIn('"bonds name "', own)
        self.assertNotIn('bonds remove', own)

    def test_label_slots_match_the_pairing_limit(self):
        config = dict(line.split('=', 1) for line in
                      (ROOT / 'prj.conf').read_text().splitlines()
                      if line.startswith('CONFIG_'))
        slots = re.search(r'#define LABEL_SLOTS (\d+)U',
                          (ROOT / 'src/bond_manager.c').read_text())
        self.assertIsNotNone(slots, 'LABEL_SLOTS is missing from the bond manager')
        self.assertEqual(int(slots.group(1)), int(config['CONFIG_BT_MAX_PAIRED']))
