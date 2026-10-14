/*
 * Adaptive baud detection for the bridge UART.
 *
 * ESP32 has hardware that measures the line but does not decide anything
 * (TRM 19.3.4, UART_AUTOBAUD_REG). Armed, it latches the shortest low and
 * shortest high pulse seen on RXD into UART_LOWPULSE and UART_HIGHPULSE, in
 * source-clock cycles. Software has to turn that into a rate, decide whether
 * to believe it, and program the divider. The arithmetic is in
 * uart_autobaud_math.h and is tested on the host; this file is the part that
 * talks to silicon and to Zephyr.
 *
 * What it deliberately does NOT do is look at the bytes. An earlier approach
 * waited for a window of mostly non-printable traffic on the theory that a
 * rate mismatch turns a console into garbage; measured on real hardware that
 * theory does not hold. The same 0x55 stream decodes as printable '!' with
 * zero unprintable bytes at one rate pair, and only 21/96 at another, so the
 * two links that most needed fixing stayed broken while the firmware waited
 * for evidence that never arrived. Byte content is a guess about the peer's
 * data. Pulse width is a measurement of its clock. Judge the line, not the
 * bytes.
 */
#include "uart_autobaud.h"

#if LINKR_UART_AUTOBAUD

#include <esp_clk_tree.h>
#include <hal/uart_hal.h>
#include <soc/uart_struct.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>

#include "uart_autobaud_math.h"

LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

/*
 * The peer's console output is bursty — a boot log peaks and then goes quiet
 * for hundreds of milliseconds — and the latches only fill while the detector
 * is armed. A window shorter than the burst period would decide on luck, so
 * this is long enough to contain at least one burst of anything a serial
 * console does. It costs nothing: an empty window is free and harmless.
 */
#define WINDOW_MS 400

/*
 * How many consecutive non-silent windows must fail to measure anything faster
 * before we trust the fastest we have seen. This is the lever against the
 * divided-rate artefact described in uart_autobaud_math.h: a single window can
 * report true_rate/k, so we wait for the window that times an actual single
 * bit and beats it.
 */
#define HOLD_WINDOWS 3

/*
 * Two full decision rounds must name the same rate before the port moves. A
 * glitch that survives the filter can produce one confident wrong answer; two
 * agreeing rounds four seconds apart are much harder to fake, and if we do
 * move wrongly, the very next rounds measure the true rate (the measurement
 * depends only on RXD, never on what we are currently set to) and move back.
 */
#define CONFIRM_ROUNDS 2

/* After moving, let the peer's line settle before judging it again. */
#define COOLDOWN_MS 1000

/* How long a deliberate `@u=` rate survives before detection takes over. */
#define HOLD_AFTER_MANUAL_MS 30000

/* Cycles below this are ignored as noise. ~200 ns at 80 MHz. */
#define GLITCH_FILT_CYCLES 16

/* A round without a single usable reading; give up rather than guess forever. */
#define REFUSED_MAX 8

static uart_hal_context_t hal_ctx;
static uint32_t src_hz;
static struct k_work_delayable autobaud_work;

/* Tightest lower bound seen in the current round, and how long it has held. */
static uint32_t round_best;
static uint32_t round_windows;
static uint32_t hold_windows;

/* What we last concluded, and how many rounds in a row said it. */
static uint32_t candidate;
static uint32_t candidate_rounds;

static uint32_t refused;

/*
 * The earliest time another decision may be taken. Used for two different
 * reasons that happen to want the same thing: a short settle after we have
 * just moved the port, and a long hold after somebody moved it on purpose.
 */
static int64_t hold_until;

/* Restart the hardware measurement. Toggling EN clears the latches. */
static void detector_rearm(void)
{
	hal_ctx.dev->auto_baud.glitch_filt = GLITCH_FILT_CYCLES;
	hal_ctx.dev->auto_baud.en = 0;
	hal_ctx.dev->auto_baud.en = 1;
}

static void detector_result(uint32_t *low, uint32_t *high, uint32_t *edges)
{
	*low = hal_ctx.dev->lowpulse.min_cnt;
	*high = hal_ctx.dev->highpulse.min_cnt;
	*edges = hal_ctx.dev->rxd_cnt.edge_cnt;
}

/* Close one round and start collecting the next. */
static void round_restart(void)
{
	round_best = 0;
	round_windows = 0;
	hold_windows = 0;
}

/*
 * One round has settled on `rate`. Decide whether that is news, whether it is
 * news we have heard before, and whether the port should move.
 */
static void round_decide(uint32_t rate)
{
	uint32_t snapped = linkr_autobaud_snap(rate);
	uint32_t current = linkr_uart_config_current();

	if (snapped == 0) {
		/* Nothing standard in reach. Mark this rate unusable rather than
		 * re-deciding it every four seconds from here on. */
		refused++;
		if (refused == REFUSED_MAX) {
			LOG_WRN("Line rate %u is not one we can follow; leaving the "
				"port at %u",
				rate, current);
		}
		candidate = 0;
		candidate_rounds = 0;
		return;
	}

	if (current != 0 && snapped == current) {
		/* Within tolerance is correct, not lucky: UART is good for a few
		 * percent, so the bytes arriving now are not a rate problem and
		 * reconfiguring would only reset the ring and discard good data. */
		candidate = 0;
		candidate_rounds = 0;
		refused = 0;
		return;
	}

	if (snapped != candidate) {
		candidate = snapped;
		candidate_rounds = 1;
		return;
	}

	candidate_rounds++;
	if (candidate_rounds < CONFIRM_ROUNDS) {
		return;
	}

	if (linkr_uart_config_reconfigure(candidate) != 0) {
		LOG_WRN("Could not move the bridge UART to %u; retrying next round",
			candidate);
		candidate_rounds = 0;
		return;
	}

	LOG_INF("Followed the peer to %u baud", candidate);
	hold_until = k_uptime_get() + COOLDOWN_MS;
	candidate = 0;
	candidate_rounds = 0;
	refused = 0;
}

static void autobaud_tick(struct k_work *work)
{
	uint32_t low;
	uint32_t high;
	uint32_t edges;
	uint32_t min_cycles;
	uint32_t rate;

	ARG_UNUSED(work);

	detector_result(&low, &high, &edges);
	detector_rearm();

	if (edges == 0) {
		/* The line did not toggle at all. That says nothing about the
		 * rate — a switched-off peer and a perfectly matched one look
		 * identical here — so it costs nothing and blocks nothing. */
		goto reschedule;
	}

	min_cycles = low < high ? low : high;
	rate = linkr_autobaud_rate(min_cycles, src_hz);

	if (rate == 0) {
		goto reschedule;
	}

	round_windows++;

	if (rate > round_best) {
		/* Something shorter than we had. Nothing previous can be trusted
		 * against this, so the count of "how long has the best held"
		 * starts over. */
		round_best = rate;
		hold_windows = 0;
		goto reschedule;
	}

	hold_windows++;
	if (hold_windows < HOLD_WINDOWS) {
		goto reschedule;
	}

	if (hold_until != 0 && k_uptime_get() < hold_until) {
		/* Almost always the 30 s after someone set the rate by hand.
		 * Keeping the round alive rather than restarting means whatever
		 * was already measured survives the wait and is judged when the
		 * hold ends, instead of starting again from nothing. */
		goto reschedule;
	}

	round_decide(round_best);
	round_restart();

reschedule:
	(void)k_work_reschedule(&autobaud_work, K_MSEC(WINDOW_MS));
}

void linkr_uart_autobaud_suspend(void)
{
	if (src_hz == 0) {
		return;
	}

	hold_until = k_uptime_get() + HOLD_AFTER_MANUAL_MS;
	round_restart();
	candidate = 0;
	candidate_rounds = 0;
	refused = 0;
}

void linkr_uart_autobaud_init(uintptr_t reg_addr)
{
	soc_module_clk_t src_clk;
	uint32_t freq = 0;
	int err;

	hal_ctx.dev = (uart_dev_t *)reg_addr;

	/*
	 * Ask what the port is actually clocked from rather than assuming APB.
	 * The ESP32 UART can run from several sources, and every rate computed
	 * below is scaled by this number: get it wrong and every window reports
	 * a confident, wrong, standard-looking rate that the port then moves to.
	 */
	uart_hal_get_sclk(&hal_ctx, &src_clk);
	err = esp_clk_tree_src_get_freq_hz(
		src_clk, ESP_CLK_TREE_SRC_FREQ_PRECISION_CACHED, &freq);
	if (err != 0 || freq == 0) {
		LOG_ERR("Cannot read the UART source clock (%d); "
			"adaptive baud detection disabled",
			err);
		return;
	}

	src_hz = freq;

	detector_rearm();
	k_work_init_delayable(&autobaud_work, autobaud_tick);
	(void)k_work_reschedule(&autobaud_work, K_MSEC(WINDOW_MS));

	LOG_INF("Adaptive baud detection armed at %u Hz source clock", src_hz);
}

#endif /* LINKR_UART_AUTOBAUD */
