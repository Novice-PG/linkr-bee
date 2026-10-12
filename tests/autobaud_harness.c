/* Host-only fakes around the production autobaud block, sliced in by
 * tests/test_autobaud.py. No Zephyr workqueue, no Espressif silicon: the
 * scenario writes the pulse counters the hardware would have latched and the
 * divider is computed the way src/main.c programs it, so every number the
 * decision sees has been through the real arithmetic. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define ARG_UNUSED(x) (void)(x)
#define K_MSEC(x) (x)
#define LOG_WRN(...) ((void)0)
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))

/*
 * 80 MHz — the source clock the divider arithmetic assumes. The clock
 * cancels out of the production formula, but a simulation still needs one to
 * turn a bit-time into cycles the way the detector would count them.
 */
#define SOURCE_HZ 80000000ull

struct k_work {
	int unused;
};

struct k_work_delayable {
	int unused;
};

typedef struct {
	struct {
		uint32_t glitch_filt;
		uint32_t en;
	} auto_baud;
	struct {
		uint32_t min_cnt;
	} lowpulse;
	struct {
		uint32_t min_cnt;
	} highpulse;
	struct {
		uint32_t div_int;
		uint32_t div_frag;
	} clk_div;
} uart_dev_t;

static uart_dev_t fake_uart;
static uint32_t fake_now_ms;
static uint32_t configured_baud = 115200;
static int reconfigure_calls;
static uint32_t reconfigured_to;

static void k_work_reschedule(struct k_work_delayable *work, int32_t ms)
{
	(void)work;
	(void)ms;
}

static void k_work_init_delayable(struct k_work_delayable *work,
				  void (*handler)(struct k_work *))
{
	(void)work;
	(void)handler;
}

static uint32_t k_uptime_get_32(void)
{
	return fake_now_ms;
}

/* What src/main.c programs: cycles-per-bit x 16, split across the integer
 * and fractional halves of clk_div — 694 + 7/16 at 115200, the numbers the
 * tick's own comment quotes. */
static void set_divider(uint32_t baud)
{
	uint32_t div = (uint32_t)(SOURCE_HZ * 16u / baud);

	fake_uart.clk_div.div_int = div >> 4;
	fake_uart.clk_div.div_frag = div & 0xFu;
}

/* The shortest run the line would latch for `bits` bit-times at `baud`. */
static uint32_t pulse(uint32_t baud, uint32_t bits)
{
	return (uint32_t)(SOURCE_HZ * bits / baud);
}

/*
 * What the detector latches when it under-counts every pulse by a fixed number
 * of cycles, which is what the real silicon does: 676 against a true 694 at
 * 115200, and 675 against 699 at 118517 — the same 18 cycles short at both. The
 * figure is constant in cycles, so its size as a fraction of a bit-time grows
 * with the rate, which is the whole reason the hold needs a band in cycles and
 * not only one in percent.
 */
#define PULSE_SHORT_CYCLES 18

static uint32_t pulse_short(uint32_t baud, uint32_t bits)
{
	return pulse(baud, bits) - PULSE_SHORT_CYCLES;
}

uint32_t linkr_uart_config_current(void);
int linkr_uart_config_reconfigure(uint32_t baudrate);

#include "autobaud_functions.inc"

uint32_t linkr_uart_config_current(void)
{
	return configured_baud;
}

int linkr_uart_config_reconfigure(uint32_t baudrate)
{
	reconfigure_calls++;
	reconfigured_to = baudrate;
	configured_baud = baudrate;
	set_divider(baudrate);

	return 0;
}

/* Close one window: hand the tick the pulse widths the detector latched and
 * let the real decision read them. */
static void window(uint32_t low, uint32_t high)
{
	fake_now_ms += 400; /* one AUTOBAUD_WINDOW_MS */
	fake_uart.lowpulse.min_cnt = low;
	fake_uart.highpulse.min_cnt = high;
	autobaud_tick(NULL);
}

/* A link that has been measured against the port, followed by a stream with
 * no single-bit run in it anywhere — 0xCC carries runs of two and three bits,
 * both polarities timing the same one, so it reads 57600 in every window
 * while the port sits at 115200. Reaching 57600 would be a guess, and the
 * line never said it. */
static void scenario_verified_then_binary(void)
{
	window(pulse(115200, 1), pulse(115200, 1));

	for (int i = 0; i < 6; i++) {
		window(pulse(115200, 2), pulse(115200, 2));
	}
}

/* Nothing has matched the port yet, so a peer that really is slower is still
 * followed — the reason the feature exists. */
static void scenario_startup_follows_a_slower_peer(void)
{
	window(pulse(9600, 1), pulse(9600, 1));
	window(pulse(9600, 1), pulse(9600, 1));
}

/* A verified link whose peer speeds up: no absent single-bit run can read
 * high, so this direction stays open — it is where the Kconfig's 1500000 /
 * 921600 / 1000000 consoles come from. */
static void scenario_verified_step_up(void)
{
	window(pulse(115200, 1), pulse(115200, 1));
	window(pulse(921600, 1), pulse(921600, 1));
	window(pulse(921600, 1), pulse(921600, 1));
}

/* Verified, then a peer that slowed to a rate our own is *not* divided by:
 * not the shape of a missing single-bit run, so it is still believed. */
static void scenario_verified_other_rate(void)
{
	configured_baud = 1500000;
	set_divider(configured_baud);

	window(pulse(1500000, 1), pulse(1500000, 1));
	window(pulse(115200, 1), pulse(115200, 1));
	window(pulse(115200, 1), pulse(115200, 1));
}

/* A window in which the line never toggled: both counters still hold the
 * power-up default the register has. This is what an idle link looks like to
 * the detector, and the only thing it can be read as. */
static void quiet_window(void)
{
	fake_now_ms += 400; /* one AUTOBAUD_WINDOW_MS */
	fake_uart.lowpulse.min_cnt = 0xFFFFFu; /* AUTOBAUD_NO_PULSE */
	fake_uart.highpulse.min_cnt = 0xFFFFFu;
	autobaud_tick(NULL);
}

/* Straight out of reset, before the line has ever been measured: a stream with
 * no single-bit run arrives. 0xCC on a true 115200 line latches two bit-times
 * on both polarities, which computes 57600 from a port configured for 115200.
 * Moving there would be a guess, and the same pulse then reads as a match at
 * the rate the port just moved to — so nothing afterwards ever disagrees with
 * it. The port has to hold. */
static void scenario_startup_binary(void)
{
	for (int i = 0; i < 20; i++) {
		window(pulse(115200, 2), pulse(115200, 2));
	}
}

/* One empty window between two bursts of the same link must not reopen the
 * refusal: a peer's bursts are ~200 ms apart and the window is 400 ms, so an
 * empty one arrives on timing alone. Followed by a stream with no single-bit
 * run, which still has to be refused. */
static void scenario_one_gap_does_not_reopen(void)
{
	window(pulse(115200, 1), pulse(115200, 1)); /* measured, agreed */
	quiet_window();
	for (int i = 0; i < 6; i++) {
		window(pulse(115200, 2), pulse(115200, 2));
	}
}

/* The refusal must be a delay, not a lock. 921600 -> 115200 is a whole
 * divisor of our own (/8), which is exactly the shape of a missing single-bit
 * run and so refused while the link is busy — but after the line has been
 * quiet long enough that it is no longer the link we measured, it is followed.
 * This is the number the reviewer's own scenario produced as still refused. */
static void scenario_quiet_then_an_integer_slowdown(void)
{
	configured_baud = 921600;
	set_divider(configured_baud);

	window(pulse(921600, 1), pulse(921600, 1)); /* measured, agreed */

	for (int i = 0; i < 10; i++) {
		quiet_window();
	}
	for (int i = 0; i < 20; i++) {
		window(pulse(115200, 1), pulse(115200, 1));
	}
}

/* The gap this closes: the line has been quiet long enough to hand the
 * evidence back, and *then* a stream with no single-bit run arrives — an idle
 * prompt followed by a binary file. 0xCC on a true 115200 line reads our own
 * rate over two bit-times in every window, which is inside the two-to-five a
 * byte stream can reach, so it is still refused after the silence. Reading
 * "the next reading is followed whether it moved or not" would take the port
 * to 57600 — a rate the line never carried — and no later window would ever
 * disagree with it again. */
static void scenario_quiet_then_a_short_binary(void)
{
	window(pulse(115200, 1), pulse(115200, 1)); /* measured, agreed */

	for (int i = 0; i < 10; i++) {
		quiet_window();
	}
	for (int i = 0; i < 20; i++) {
		window(pulse(115200, 2), pulse(115200, 2));
	}
}

/* The reading this link actually produced: a verified 115200 link whose
 * windows all latch a shortest pulse a whisker short of one bit-time
 * (measured on hardware: 676 cycles against the true 694 — a sampling
 * artifact, not a rate). That computes 118342, 2.7% high: close enough that
 * the port should be held rather than moved, because chasing it cost a real
 * board its data — the move reset the ring (33 bytes gone mid-transfer) and
 * mis-sampling at the new rate cost ~3.5% of every byte after it, and the
 * wrong port kept re-measuring as itself so nothing ever put it back. */
static void scenario_artifact_high_is_refused(void)
{
	window(pulse(115200, 1), pulse(115200, 1)); /* measured, agreed */

	for (int i = 0; i < 6; i++) {
		window(676, 676);
	}
}

/* The other side of the same rule, and the reason it is a bound on the gap
 * rather than a list of blessed rates: a peer that really is at a rate the
 * snap table does not know must still be followed. 150000 is 30% above our
 * 115200 — a real move, far outside any band we hold — and it is not a
 * divisor of ours, so two agreeing windows take the port there exactly as
 * they always did. Same for a console that boots any other odd rate: the
 * hold is about how *small* a change is worth acting on, never about which
 * rates are allowed. */
static void scenario_odd_rate_peer_is_followed(void)
{
	window(pulse(115200, 1), pulse(115200, 1)); /* measured, agreed */

	for (int i = 0; i < 4; i++) {
		window(pulse(150000, 1), pulse(150000, 1));
	}
}

/* The way out if a port is ever found at a wrong rate anyway — what the real
 * board looked like after the bug moved it: nothing has measured the port at
 * 118342, so it keeps no claim to be trusted, the hold does not apply, and
 * the first clean windows read the true rate (694 cycles -> 115200), which is
 * neither within the band of a trustworthy port nor a divisor shape, so two
 * of them put the port back. This is the rollback the two-window rule always
 * promised. */
static void scenario_stuck_port_recovers(void)
{
	/* Drop the link's measured state the way the move that mis-set it
	 * would: a reconfiguration clears verification, and so does sustained
	 * silence. */
	for (int i = 0; i < 10; i++) {
		quiet_window();
	}
	configured_baud = 118342;
	set_divider(configured_baud);

	for (int i = 0; i < 8; i++) {
		window(pulse(115200, 1), pulse(115200, 1)); /* clean */
	}
}

/* The reading a hold measured only in percent cannot reach: a port at 230400
 * whose detector is short by the same 18 cycles it is short by at 115200.
 * Eighteen cycles is 5.3% of the 347 the port is programmed for, so a 5%
 * hold-by-rate misses it by a hair and the port would move — to 243143, a rate
 * nothing is at. Only the band in cycles holds it. At 921600 the same defect
 * is 21% and the percentage band is not even close. */
static void scenario_short_pulse_at_high_rate_is_held(void)
{
	configured_baud = 230400;
	set_divider(configured_baud);

	for (int i = 0; i < 6; i++) {
		window(pulse_short(230400, 1), pulse_short(230400, 1));
	}
}

/* The other side of the cycle band: a real move is hundreds of cycles away,
 * never eighteen, so the band never holds a peer that actually changed rate.
 * 150000 arriving on a 115200 port is 161 cycles from what the port is
 * programmed for — six times the band's width — so it is followed exactly as
 * it always was, detector error and all. */
static void scenario_a_short_pulse_does_not_hold_a_real_move(void)
{
	for (int i = 0; i < 6; i++) {
		window(pulse_short(150000, 1), pulse_short(150000, 1));
	}
}

int main(int argc, char **argv)
{
	const char *scenario = argc > 1 ? argv[1] : "";

	/* The production init itself, not a stand-in: it arms the detector,
	 * starts the window, and holds the cooldown and warning clocks. */
	set_divider(configured_baud);
	linkr_uart_autobaud_init((uintptr_t)&fake_uart);

	if (strcmp(scenario, "verified_then_binary") == 0) {
		scenario_verified_then_binary();
	} else if (strcmp(scenario, "startup_follows_a_slower_peer") == 0) {
		scenario_startup_follows_a_slower_peer();
	} else if (strcmp(scenario, "verified_step_up") == 0) {
		scenario_verified_step_up();
	} else if (strcmp(scenario, "verified_other_rate") == 0) {
		scenario_verified_other_rate();
	} else if (strcmp(scenario, "startup_binary") == 0) {
		scenario_startup_binary();
	} else if (strcmp(scenario, "one_gap_does_not_reopen") == 0) {
		scenario_one_gap_does_not_reopen();
	} else if (strcmp(scenario, "quiet_then_an_integer_slowdown") == 0) {
		scenario_quiet_then_an_integer_slowdown();
	} else if (strcmp(scenario, "quiet_then_a_short_binary") == 0) {
		scenario_quiet_then_a_short_binary();
	} else if (strcmp(scenario, "artifact_high_is_refused") == 0) {
		scenario_artifact_high_is_refused();
	} else if (strcmp(scenario, "odd_rate_peer_is_followed") == 0) {
		scenario_odd_rate_peer_is_followed();
	} else if (strcmp(scenario, "stuck_port_recovers") == 0) {
		scenario_stuck_port_recovers();
	} else if (strcmp(scenario, "short_pulse_at_high_rate_is_held") == 0) {
		scenario_short_pulse_at_high_rate_is_held();
	} else if (strcmp(scenario,
			  "a_short_pulse_does_not_hold_a_real_move") == 0) {
		scenario_a_short_pulse_does_not_hold_a_real_move();
	} else {
		fprintf(stderr, "unknown scenario: %s\n", scenario);
		return 2;
	}

	printf("rate=%u\n", configured_baud);
	printf("reconfigs=%d\n", reconfigure_calls);
	printf("last=%u\n", reconfigured_to);
	printf("verified=%d\n", autobaud_verified ? 1 : 0);

	return 0;
}
