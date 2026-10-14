/*
 * Host-side tests for the rate arithmetic in src/uart_autobaud_math.h.
 *
 * Built with a plain compiler and no Zephyr:
 *
 *     cc -std=c11 -Wall -Wextra -Werror -o /tmp/t tests/test_uart_autobaud.c
 *     /tmp/t
 *
 * The reason this file exists rather than trusting the firmware to be right:
 * the failure mode is silent. A wrong rate here reconfigures a UART that was
 * working, discards whatever was in flight, and presents to whoever is holding
 * the board as "the wireless link dropped" — not as "the firmware guessed a
 * number". Every assertion below is about a boundary someone could plausibly
 * cross without noticing.
 */
#include "../src/uart_autobaud_math.h"

#include <stdio.h>

static int failures;

static void check(int ok, const char *what, unsigned long a, unsigned long b)
{
	if (ok) {
		return;
	}
	failures++;
	printf("FAIL %s (got %lu, expected %lu)\n", what, a, b);
}

/* The ESP32's default UART source clock. */
#define APB 80000000u

static void within_is_symmetric_and_exclusive(void)
{
	/* 3% of 119000 is 3570, so 3800 short of it is outside; 3% of 115200 is
	 * 3456, so 3200 short of that is inside. Both are meant to be near the
	 * boundary, because that is where a wrong comparison shows up. */
	check(linkr_autobaud_within(115200u, 119000u, 3u) == false,
	      "3800 apart at 119000 is more than 3%", 1, 0);
	check(linkr_autobaud_within(115200u, 112000u, 3u) == true,
	      "3200 apart at 115200 is within 3%", 0, 0);

	/* Symmetric: which operand is "the measurement" must not matter. */
	check(linkr_autobaud_within(112000u, 115200u, 3u) ==
		      linkr_autobaud_within(115200u, 112000u, 3u),
	      "within() depends on operand order", 1, 0);

	/* Zero is not "within" anything, including itself. An un-timed latch
	 * reading is usually that, rather than a line running at zero baud. */
	check(linkr_autobaud_within(0u, 0u, 3u) == false,
	      "zero must not compare as a match", 1, 0);
}

static void pulses_become_rates(void)
{
	/* One bit time at 115200 and 80 MHz is 694 cycles. */
	check(linkr_autobaud_rate(694u, APB) >= 115000u &&
		      linkr_autobaud_rate(694u, APB) <= 115300u,
	      "694 cycles at 80 MHz", linkr_autobaud_rate(694u, APB), 115200u);

	/* An idle detector reads 0. That is not a rate. */
	check(linkr_autobaud_rate(0u, APB) == 0u, "un-timed latch must read as 0",
	      linkr_autobaud_rate(0u, APB), 0u);

	/* One cycle is nonsense: 80 Mbaud. Above anything we can set. */
	check(linkr_autobaud_rate(1u, APB) == 0u, "one cycle is not a line rate",
	      linkr_autobaud_rate(1u, APB), 0u);

	/* Below the floor we'd ever be asked to follow: 300 baud needs 266666
	 * cycles, so one more than that is slower than any peer. */
	check(linkr_autobaud_rate(266667u, APB) == 0u,
	      "below 300 baud is refused", linkr_autobaud_rate(266667u, APB), 0u);
	check(linkr_autobaud_rate(266666u, APB) == 300u,
	      "266666 cycles is 300 baud", linkr_autobaud_rate(266666u, APB), 300u);
}

/*
 * On a line carrying one-bit runs the two polarities latch the same width, so
 * anything wider than the agree margin is one side being wrong — and the rate
 * reduction takes the smaller reading, which is the one a spike writes. Every
 * case here is about that: a window either says "nothing happened" or it is
 * discarded, but it is never allowed to become a rate.
 */
static void polarities_must_agree(void)
{
	/* Exactly equal, as measured on a real line (8319/8319, 692/692). */
	check(linkr_autobaud_apart(692u, 692u) == false,
	      "identical polarities are one window", 1, 0);

	/* The detector's own rounding: 26 cycles off a 694-cycle bit is 3.7%,
	 * still inside the 5% margin, so this is one measurement. */
	check(linkr_autobaud_apart(694u, 720u) == false,
	      "3.7% apart still agrees", 1, 0);

	/* Exactly 5% still agrees — the rule is wider than, not equal to. */
	check(linkr_autobaud_apart(1000u, 1050u) == false,
	      "exactly 5% apart still agrees", 1, 0);
	check(linkr_autobaud_apart(1000u, 1051u) == true,
	      "just over 5% apart does not", 1, 0);

	/* The case that matters. An 80-cycle spike on a 694-cycle bit reads as
	 * 1 Mbaud, snaps cleanly to 1000000, and then holds: every honest
	 * window after it is slower, so those only count as agreement. */
	check(linkr_autobaud_apart(80u, 694u) == true,
	      "a glitch-sized pulse against a real bit is refused", 1, 0);

	/* What stty moving the divisor mid-byte measured: 461 against 692, a
	 * pulse cut in half. */
	check(linkr_autobaud_apart(461u, 692u) == true,
	      "a half pulse against a whole one is refused", 1, 0);

	/* A polarity that saw nothing is a window that only half happened. */
	check(linkr_autobaud_apart(0u, 694u) == true,
	      "one silent polarity is not agreement", 1, 0);

	/* Both silent is silence, which the caller handles before asking. */
	check(linkr_autobaud_apart(0u, 0u) == false,
	      "both silent is silence, not disagreement", 1, 0);

	/* Whichever way round they are written. */
	check(linkr_autobaud_apart(692u, 461u) ==
		      linkr_autobaud_apart(461u, 692u),
	      "apart() depends on operand order", 1, 0);
}

/*
 * A reading that is ourselves divided by a whole factor is a window with no
 * one-bit run in it. Every bound below comes out of linkr_autobaud_within()
 * run over current/k for k in 2..5, so the cases pin both edges of each window
 * rather than quoting its centre — a rule only worth having if it turns exactly
 * where it says it turns.
 */
static void divisors_of_ourselves_are_not_a_peer(void)
{
	/* The measured artefact itself: 0xCC at 115200 leaves runs of two and
	 * three bits and computes 57596, which snaps to 57600 and would then
	 * confirm itself forever. */
	check(linkr_autobaud_divisor_artefact(57600u, 115200u) == true,
	      "half of ourselves is the artefact", 1, 0);
	check(linkr_autobaud_divisor_artefact(57596u, 115200u) == true,
	      "the value before snapping is too", 1, 0);

	/* Top of the half window: 97 * R <= 5760000 under the symmetric rule. */
	check(linkr_autobaud_divisor_artefact(59381u, 115200u) == true,
	      "the top edge of the half window still counts", 1, 0);
	check(linkr_autobaud_divisor_artefact(59382u, 115200u) == false,
	      "one unit past the top edge does not", 1, 0);

	/* Bottom: current/k may sit up to 3% above the reading, and 3% of 57600
	 * is 1728, so 55872 is the last one. */
	check(linkr_autobaud_divisor_artefact(55872u, 115200u) == true,
	      "the bottom edge of the half window still counts", 1, 0);
	check(linkr_autobaud_divisor_artefact(55871u, 115200u) == false,
	      "one unit below the bottom edge does not", 1, 0);

	/* Whole factors through the range a framed byte can fake. Five is the
	 * limit: five low and five high is the longest both polarities reach
	 * together without a one-bit run in the window. */
	check(linkr_autobaud_divisor_artefact(38400u, 115200u) == true,
	      "a third of ourselves counts", 1, 0);
	check(linkr_autobaud_divisor_artefact(23040u, 115200u) == true,
	      "a fifth of ourselves counts", 1, 0);

	/* Past five no byte stream can fake it, so those stay followable —
	 * including a sixth, which is the shape a 115200 console dropped to
	 * 19200 arrives in. */
	check(linkr_autobaud_divisor_artefact(19200u, 115200u) == false,
	      "a sixth of ourselves stays followable", 1, 0);
	check(linkr_autobaud_divisor_artefact(9600u, 115200u) == false,
	      "115200 to 9600, the case detection exists for", 1, 0);

	/* Not a divisor: our own rate, and a peer that got faster. Neither is
	 * shaped like a missing one-bit run, so neither is refused. */
	check(linkr_autobaud_divisor_artefact(115200u, 115200u) == false,
	      "our own rate is not its own artefact", 1, 0);
	check(linkr_autobaud_divisor_artefact(230400u, 115200u) == false,
	      "a peer that sped up cannot be a divisor of us", 1, 0);
	check(linkr_autobaud_divisor_artefact(50000u, 115200u) == false,
	      "a rate that fits no factor is followed", 1, 0);

	/* A port whose rate is not known has no divisors to stand on. */
	check(linkr_autobaud_divisor_artefact(57600u, 0u) == false,
	      "an unknown current rate refuses nothing", 1, 0);
}

static void snapping_names_one_rate_or_none(void)
{
	check(linkr_autobaud_snap(115200u) == 115200u, "exact 115200",
	      linkr_autobaud_snap(115200u), 115200u);
	check(linkr_autobaud_snap(115000u) == 115200u, "0.17% low still 115200",
	      linkr_autobaud_snap(115000u), 115200u);

	/* 1.5 Mbaud is a real SBC console rate and must not be snapped to its
	 * neighbour just because we listed them in order. */
	check(linkr_autobaud_snap(1500000u) == 1500000u, "exact 1.5M",
	      linkr_autobaud_snap(1500000u), 1500000u);
	check(linkr_autobaud_snap(1490000u) == 1500000u, "0.7% low still 1.5M",
	      linkr_autobaud_snap(1490000u), 1500000u);

	/* 74880 is what an ESP32 prints from its ROM loader and 14400 is a rate
	 * between the ones a terminal program offers; both were missing from
	 * the table, so a peer running either would have been refused instead
	 * of followed. */
	check(linkr_autobaud_snap(74880u) == 74880u, "exact 74880",
	      linkr_autobaud_snap(74880u), 74880u);
	check(linkr_autobaud_snap(74000u) == 74880u, "1.2% low still 74880",
	      linkr_autobaud_snap(74000u), 74880u);
	check(linkr_autobaud_snap(14400u) == 14400u, "exact 14400",
	      linkr_autobaud_snap(14400u), 14400u);
	check(linkr_autobaud_snap(14000u) == 14400u, "2.8% low still 14400",
	      linkr_autobaud_snap(14000u), 14400u);

	/* The new entry must not reach up and take 115200, and the gap it
	 * opened between itself and 115200 must be a place a measurement can
	 * land and still be refused. */
	check(linkr_autobaud_snap(90000u) == 0u,
	      "between 74880 and 115200 is refused", linkr_autobaud_snap(90000u),
	      0u);

	/* Deliberately between two entries: 480000 sits between 460800 and
	 * 500000, 4.2% from the first and 4.2% from the second. Neither is
	 * "nearest" in any meaningful sense, so refuse. */
	check(linkr_autobaud_snap(480000u) == 0u, "halfway entries are refused",
	      linkr_autobaud_snap(480000u), 0u);

	check(linkr_autobaud_snap(0u) == 0u, "zero is not a rate",
	      linkr_autobaud_snap(0u), 0u);
	check(linkr_autobaud_snap(12345678u) == 0u, "absurd rates are refused",
	      linkr_autobaud_snap(12345678u), 0u);
}

/*
 * Every adjacent pair in the accepted list must be further apart than twice
 * the tolerance, or one measurement could be "within tolerance" of two rates
 * and the choice would come down to list order — which is not a decision, it
 * is an accident waiting for someone to reorder the table.
 */
static void the_list_has_no_ambiguous_neighbours(void)
{
	uint32_t previous = 0;

	for (uint32_t rate = LINKR_AUTOBAUD_MIN_RATE;
	     rate <= LINKR_AUTOBAUD_MAX_RATE; rate++) {
		uint32_t snapped = linkr_autobaud_snap(rate);

		if (previous != 0 && snapped != previous) {
			check(linkr_autobaud_within(previous, snapped,
						    2u * LINKR_AUTOBAUD_TOLERANCE_PCT) ==
				      false,
			      "two adjacent rates can both match one measurement",
			      previous, snapped);
		}

		if (snapped != 0) {
			previous = snapped;
		}
	}
}

int main(void)
{
	within_is_symmetric_and_exclusive();
	polarities_must_agree();
	divisors_of_ourselves_are_not_a_peer();
	pulses_become_rates();
	snapping_names_one_rate_or_none();
	the_list_has_no_ambiguous_neighbours();

	if (failures != 0) {
		printf("%d assertion(s) failed\n", failures);
		return 1;
	}

	printf("uart_autobaud_math: all assertions hold\n");
	return 0;
}
