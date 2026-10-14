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
