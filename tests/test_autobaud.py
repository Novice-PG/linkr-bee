"""Run with python3 -m unittest discover -s tests -v (Python + host C compiler).

The pulse widths a window reports are the *only* thing the autobaud decision
gets to look at, so they are the only thing a host test has to supply: the
harness runs the production tick against counts computed from a chosen line
rate, and asks what the port ended up configured to.
"""

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


def extract(source, start_marker, end_marker, required):
    """Slice a production block by literal anchors.

    A moved anchor should fail loudly rather than quietly compile a different
    function, so the intended definitions are asserted to be inside the block.
    Truncation itself cannot pass unnoticed: the harness compiles with -Werror.
    """
    start = source.index(start_marker)
    block = source[start:source.index(end_marker, start)]
    for name in required:
        if f"{name}(" not in block:
            raise AssertionError(f"{name} is missing from the extracted block")
    return block


class AutobaudTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="linkr-autobaud-")
        cls.addClassCleanup(cls.tmp.cleanup)
        directory = Path(cls.tmp.name)
        production = (ROOT / "src/uart_autobaud.c").read_text()
        block = extract(
            production,
            "#define AUTOBAUD_WINDOW_MS",
            "\n#endif /* LINKR_UART_AUTOBAUD */",
            ["autobaud_tick", "autobaud_reads_like_a_missing_bit",
             "autobaud_snap", "autobaud_forget", "autobaud_apart",
             "autobaud_pulse_within_cycles", "linkr_uart_autobaud_init"],
        )
        (directory / "autobaud_functions.inc").write_text(block)
        cls.binary = directory / "autobaud-tests"
        subprocess.run([
            *shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall",
            "-Wextra", "-Werror", "-I", str(directory),
            str(ROOT / "tests/autobaud_harness.c"), "-o", str(cls.binary),
        ], check=True)

    def run_scenario(self, scenario):
        result = subprocess.run([str(self.binary), scenario], check=True,
                                capture_output=True, text=True, timeout=5)
        return dict(line.split("=", 1)
                    for line in result.stdout.splitlines() if "=" in line)

    def test_a_verified_link_is_not_moved_by_a_stream_without_single_bit_runs(self):
        """The one this exists for: 0xCC must not reach 57600.

        The link has been measured against the port at 115200, then a stream
        whose shortest runs are two bit-times arrives in every window. That
        reads as 57600 — our own rate over two — and it reads as 57600 in
        every window afterwards too, because the width belongs to the line and
        not to our divisor, so a reconfiguration would have no later window
        left to undo it with. Six such windows: the port must not move.
        """
        result = self.run_scenario("verified_then_binary")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "0")

    def test_before_anything_has_matched_a_slower_peer_is_still_followed(self):
        """A slower peer is still followed, which is the reason autobaud exists.

        The link starts out verified — Kconfig's rate is what both ends were
        set to — but verification only refuses readings *below* ours that are
        our own rate over 2..9 bit-times, the shape a missing single-bit run
        leaves. 9600 against a 115200 port is not that shape (/12), so two
        agreeing windows move the port. Refusing *this* would have meant
        refusing the reason autobaud exists.
        """
        result = self.run_scenario("startup_follows_a_slower_peer")
        self.assertEqual(result["rate"], "9600")
        self.assertEqual(result["reconfigs"], "1")
        self.assertEqual(result["last"], "9600")

    def test_a_verified_link_still_follows_a_peer_that_speeds_up(self):
        """A missing single-bit run can only ever read low, never high.

        So the upward direction — where the Kconfig's 1500000 / 921600 /
        1000000 console rates come from — is left exactly as it was.
        """
        result = self.run_scenario("verified_step_up")
        self.assertEqual(result["rate"], "921600")
        self.assertEqual(result["reconfigs"], "1")
        self.assertEqual(result["last"], "921600")

    def test_a_verified_link_still_follows_a_slowdown_it_cannot_be_an_artifact_of(self):
        """The refusal is the shape of a missing run, not "slower" in general.

        1500000 down to 115200 is not our own rate divided by anything from 2
        to 9, so nothing about it is explainable by the longest run a framed
        byte can leave, and the reading is believed.
        """
        result = self.run_scenario("verified_other_rate")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "1")
        self.assertEqual(result["last"], "115200")


    def test_a_binary_stream_from_reset_does_not_move_the_port(self):
        """The entry the existing tests never covered: nothing matched yet.

        0xCC arriving on a line that really is 115200 latches two bit-times on
        both polarities, which computes 57600 from a port configured for
        115200. Moving there is a guess — and the same pulse then reads as a
        match at the rate the port just moved to, so no later window ever
        disagrees with it and the link stays broken. Twenty windows of it: the
        configured rate is the working assumption until the line says
        otherwise, so the port holds and `verified` stays set (which is what
        keeps the refusal armed for every one of those windows).
        """
        result = self.run_scenario("startup_binary")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "0")
        self.assertEqual(result["verified"], "1")

    def test_one_empty_window_does_not_reopen_a_refused_reading(self):
        """A gap is not a rate change: the peer's bursts are 200 ms apart.

        A single empty window can arrive on timing alone, so reopening on one
        would drop the protection between two bursts of the very same link —
        which is the next line in this scenario, a stream with no single-bit
        run that still has to be refused.
        """
        result = self.run_scenario("one_gap_does_not_reopen")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "0")

    def test_a_refused_slowdown_is_followed_once_the_line_has_been_quiet(self):
        """The refusal is a delay, not a lock.

        921600 -> 115200 is a whole divisor of our own (/8), which is exactly
        the shape of a missing single-bit run, so while the link is busy there
        is no reading that tells it apart from one and it is refused. After
        the line has been quiet for longer than a burst gap it is no longer
        the link that was measured, and the next reading is believed: the
        silence, not the pulse width, is what reopens it. Without that this
        direction was unreachable — 921600 and 115200 stayed as they were
        forever.
        """
        result = self.run_scenario("quiet_then_an_integer_slowdown")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "1")
        self.assertEqual(result["last"], "115200")

    def test_a_quiet_line_does_not_let_a_short_binary_move_the_port(self):
        """The silence hands back only the divisors a stream cannot fake.

        An idle prompt is quiet for far longer than a burst gap, so it drops
        the link's measured state exactly as a peer's rate change does. What
        must not follow is a stream with no single-bit run in it: 0xCC on a
        true 115200 line reads our own rate over two bit-times, and two is
        inside the two-to-five a byte stream can leave, so it is refused even
        here. Following it would take the port to 57600, and the same pulse
        would then read as a match at the rate it just moved to, so nothing
        later could ever put it back. This is the case an idle line followed
        by a file dump produces; before the two-to-five bound it reached 57600
        and stayed there.
        """
        result = self.run_scenario("quiet_then_a_short_binary")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "0")

    def test_an_artifact_reading_above_our_rate_is_refused(self):
        """The reading this link really produced: 2.7% high, held not chased.

        A 115200 line that was being mis-sampled latched a shortest pulse of
        676 cycles against the true 694 in window after window, which
        computes 118342 — and the two-window rule moved a real board there.
        The move cost the ring reset (33 bytes gone mid-transfer) and ~3.5%
        of every byte after, and the wrong port then kept re-measuring as
        itself, so nothing ever put it back. A reading this close to our own
        rate says nothing worth acting on: it is our own rate measured short.
        """
        result = self.run_scenario("artifact_high_is_refused")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "0")

    def test_a_peer_at_an_odd_rate_is_still_followed(self):
        """The hold is a bound on the gap, not a list of blessed rates.

        If the peer really is at a rate the snap table does not know — 150000,
        say, 30% above our own — the bridge must follow it exactly as it
        always did, or swapping to that peer leaves it deaf, which is the
        failure autobaud exists to prevent. Nothing about being "non-standard"
        refuses a reading.
        """
        result = self.run_scenario("odd_rate_peer_is_followed")
        # pulse() truncates 80e6/150000 to 533 cycles, so the harness computes
        # 150092 from it — a tenth of a percent off in the harness's own
        # arithmetic. What matters here is the property, not the digit: the
        # port left 115200 for a far, non-standard rate and stayed there.
        self.assertEqual(result["reconfigs"], "1")
        self.assertNotEqual(result["rate"], "115200")
        self.assertEqual(result["rate"], result["last"])
        self.assertGreater(int(result["rate"]), 115200 * 11 // 10)

    def test_a_port_found_at_a_wrong_rate_is_put_back(self):
        """What the real board looked like, and the way out.

        A port at 118342 that nothing has measured keeps no claim to be
        trusted, so the hold does not protect it: the first clean windows read
        the true rate (694 cycles -> 115200), which two agreeing windows
        follow, and the port comes back with `verified` re-armed. This is the
        rollback the two-window rule always promised, and the reason the hold
        is conditioned on a trustworthy port rather than applied blindly.
        """
        result = self.run_scenario("stuck_port_recovers")
        self.assertEqual(result["rate"], "115200")
        self.assertEqual(result["reconfigs"], "1")
        self.assertEqual(result["last"], "115200")
        self.assertEqual(result["verified"], "1")

    def test_a_short_pulse_at_a_high_rate_is_held_without_a_percentage_band(self):
        """The detector's error is fixed in cycles, so percent alone loses it.

        A 230400 port whose detector latches 329 cycles where the true pulse is
        347 — the same 18 cycles short it is short by at 115200. Eighteen
        cycles is 5.3% of a bit-time here, so a hold of five percent by rate
        misses it by a hair; at 921600 the same defect is 21% and the rate band
        is not even close. The band in cycles is what covers the difference: a
        port that moved would go to 243143, a rate no peer is at, and the same
        short pulse would then read as a match at the rate it just moved to, so
        nothing would ever put it back.
        """
        result = self.run_scenario("short_pulse_at_high_rate_is_held")
        self.assertEqual(result["rate"], "230400")
        self.assertEqual(result["reconfigs"], "0")

    def test_a_short_pulse_never_holds_a_real_move(self):
        """The cycle band is a bound on the gap, exactly like the percentage one.

        150000 arriving on a 115200 port moves the pulse 161 cycles from what
        the port is programmed for — six times the band's width — so the port
        is still taken to the peer's rate as it always was, detector error and
        all. If this ever fails, the band has grown wide enough to make the
        bridge deaf to a peer that really did change rate, which is the failure
        autobaud exists to prevent.
        """
        result = self.run_scenario("a_short_pulse_does_not_hold_a_real_move")
        self.assertEqual(result["reconfigs"], "1")
        self.assertNotEqual(result["rate"], "115200")
        self.assertEqual(result["rate"], result["last"])


if __name__ == "__main__":
    unittest.main()
