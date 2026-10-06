/*
 * BACKLOG M5 — follow the peer's line rate instead of demanding it match us.
 *
 * The port is configured once from CONFIG_LINKR_BLE_BRIDGE_UART_BAUD_RATE and
 * never questioned, while that same Kconfig's help lists 1500000 / 921600 /
 * 1000000 as typical SBC console rates: swap the SBC and the bridge goes deaf
 * with nothing in the firmware noticing. "Linkr adapts to the host" is the
 * direction that has to hold — the SBC must not be asked to change a single
 * stty to suit this board.
 *
 * ESP32 has a hardware autobaud detector (TRM 19.3.4, UART_AUTOBAUD_REG), but
 * it only measures. Armed, it records the shortest low and high pulse seen on
 * RXD into UART_LOWPULSE / UART_HIGHPULSE in source-clock cycles. Software has
 * to turn min(low, high) into a rate, decide whether to believe it, and
 * program the divider — that is what the rest of this file does.
 *
 * How it decides to look: it does not read the traffic at all. An earlier
 * version waited for a window of mostly non-printable bytes, because a rate
 * mismatch is supposed to turn a console into garbage. Measured on real
 * hardware, that assumption does not hold — the same 0x55 test stream decoded
 * as 0x21 ('!') with *zero* unprintable bytes at 19200 vs 115200, and as only
 * 21/96 at 115200 vs 921600, while 50-52% cases sat right on the threshold.
 * Those two links stayed broken and the firmware never noticed. Byte content
 * is a guess; the pulse width is a measurement, so the detector is armed
 * continuously and every window is judged on what the line actually did.
 *
 * The window is 400 ms rather than a few tens: the peer's traffic is bursty
 * (measured: a burst about every 200 ms) and the latches only fill while the
 * detector is armed, so a shorter window can come back empty and a window
 * shorter than the burst period would depend on luck. An empty window is not
 * a failure — it means the line has not toggled at all since the window
 * opened, which says nothing about the rate — so it costs nothing and never
 * blocks a later answer. (A stream of 0x00 or 0xFF is not empty: its stop bit
 * still leaves a one-bit high run, so that case measures fine.)
 *
 * Two windows must agree before the port is touched. A single window can be
 * wrong: a glitch that survives the filter, or a byte with no single-bit run
 * (only bytes >= 0x80 with a zero LSB manage that, so it needs a whole window
 * of them), would report half or a third of the real rate. The line does not
 * change when we guess badly, so the next windows measure the true rate again
 * and put it back — the measurement depends only on RXD, never on what the
 * port is currently set to.
 *
 * It leaves everything alone when the measurement lands within 2% of what we
 * already have. UART is good for that much, so the bytes were not a rate
 * problem and reconfiguring would only reset the ring and throw away good
 * data.
 */

#include "uart_autobaud.h"

#if LINKR_UART_AUTOBAUD

#include <soc/uart_struct.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/util.h>

/* Shares main.c's log source: same prefix, no second source slot in RAM. */
LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

#define AUTOBAUD_WINDOW_MS	400 /* >= one peer burst period (measured 200) */
#define AUTOBAUD_CONFIRM	2	  /* agreeing windows before touching it */
#define AUTOBAUD_COOLDOWN_MS	1000 /* after a change, let the line settle */
#define AUTOBAUD_REFUSED_MAX	6	  /* disagreeing windows before saying so */
/*
 * lowpulse and highpulse both hold the shortest run of their own polarity, so
 * on any line carrying one-bit runs they land on the same number of source
 * cycles and differ only by rounding — measured as exactly equal (8319/8319,
 * 692/692, 4159/4159). Anything wider means one side is wrong and a single
 * window cannot say which, so it is not a measurement worth acting on. The
 * only way to get here with a peer holding mid-byte is stty changing the
 * divisor while its own bytes are still queued: the pulse is cut in half and
 * the two polarities land anywhere (measured: 461/692, 1776/4159).
 */
#define AUTOBAUD_AGREE_PCT	5
#define AUTOBAUD_GLITCH_FILT	16 /* cycles, ~200 ns at 80 MHz */
#define AUTOBAUD_TOLERANCE_PCT	2 /* UART itself is good for ~2-3% */
#define AUTOBAUD_MIN_BAUD	300	   /* mirrors Kconfig's range */
#define AUTOBAUD_MAX_BAUD	3000000 /* mirrors Kconfig's range */
/*
 * Both counters power up, and re-arm, at 0xFFFFF — their documented default,
 * meaning no pulse of that polarity has been timed since. An idle line reads
 * exactly that. It is the absence of a measurement rather than a measurement
 * of 13 ms, so it must be dropped silently instead of being divided into a
 * baud rate and reported as one (1048575 cycles/bit reads back as 76 baud,
 * which is what spammed the console until this was matched against the
 * register default).
 */
#define AUTOBAUD_NO_PULSE	0xFFFFFu
/* Implausible readings can repeat as fast as the windows do — a noise spike
 * that slips past the 16-cycle filter lands above MAX_BAUD, an all-constant
 * frame below MIN_BAUD — so they are reported at most this often. */
#define AUTOBAUD_WARN_MS	5000

static struct k_work_delayable autobaud_work;
static uintptr_t autobaud_reg;
static uint32_t autobaud_candidate;	  /* rate seen, waiting for a repeat */
static uint32_t autobaud_cooldown_until;
static uint32_t autobaud_warned_at;
static uint8_t autobaud_confirm;		  /* windows agreeing on it */
static uint8_t autobaud_refused;

static uart_dev_t *autobaud_hw(void)
{
	return (uart_dev_t *)autobaud_reg;
}

/*
 * Forget the pending candidate. Called whenever the line says something else:
 * a rate is only acted on after being read twice with nothing in between.
 */
static void autobaud_forget(void)
{
	autobaud_confirm = 0;
	autobaud_candidate = 0;
}

/*
 * True when the two polarities did not time the same shortest run, which is
 * the only state in which one window cannot be trusted.
 */
static bool autobaud_apart(uint32_t a, uint32_t b)
{
	uint32_t lo = MIN(a, b);
	uint32_t hi = MAX(a, b);

	return (hi - lo) * 100u > lo * AUTOBAUD_AGREE_PCT;
}

/* Rounds to a rate worth reporting when the measurement is a whisker off. */
static uint32_t autobaud_snap(uint32_t measured)
{
	static const uint32_t standard[] = {
		9600,    14400,   19200,   38400,	 57600,  74880,
		115200,  230400,  460800,  921600,	1000000, 1500000,
	};

	for (size_t i = 0; i < ARRAY_SIZE(standard); i++) {
		uint32_t s = standard[i];
		uint32_t delta = measured > s ? measured - s : s - measured;

		if (delta * 100u <= s) { /* within 1% */
			return s;
		}
	}
	return measured;
}

static void autobaud_arm(uart_dev_t *hw)
{
	/*
	 * Discard the last latched widths and start timing again. auto_baud.en
	 * is the one thing no amount of polling can substitute for: without it
	 * both pulse counters stay at zero and every window reports nothing
	 * usable no matter what the peer does (which is exactly how this failed
	 * the first time).
	 */
	hw->auto_baud.glitch_filt = AUTOBAUD_GLITCH_FILT;
	hw->auto_baud.en = 0;
	hw->auto_baud.en = 1;
}

/*
 * Runs on the system workqueue but never blocks it: each visit reads the
 * window that just closed, re-arms for the next one, and either stops there
 * or reprograms the port. No sleeping, so the workqueue stays free for
 * everything else — and nothing here needs more than a handful of registers,
 * so dram1 does not grow by a thread.
 */
static void autobaud_tick(struct k_work *work)
{
	uart_dev_t *hw = autobaud_hw();
	uint32_t now = k_uptime_get_32();
	uint32_t low, high, old_baud, div_meas, div_cur, next, lo, hi;
	uint64_t measured;

	ARG_UNUSED(work);

	/*
	 * Latch the widths BEFORE clearing AUTOBAUD_EN: disabling the detector
	 * may reset the very counters being read, and a zero here would be
	 * indistinguishable from "the line never toggled at all".
	 */
	low = hw->lowpulse.min_cnt;
	high = hw->highpulse.min_cnt;

	/* Close this window and open the next one back to back, so no burst
	 * can fall in a gap between them. */
	autobaud_arm(hw);

	k_work_reschedule(&autobaud_work, K_MSEC(AUTOBAUD_WINDOW_MS));

	if ((int32_t)(now - autobaud_cooldown_until) < 0) {
		return; /* a change of ours is still settling */
	}

	if (low == 0 || high == 0 || low >= AUTOBAUD_NO_PULSE ||
	    high >= AUTOBAUD_NO_PULSE) {
		/* Nothing to time, and nothing that says anything about the
		 * rate — so it must not eat the refusal budget either. */
		return;
	}

	if (autobaud_apart(low, high)) {
		autobaud_forget();
		if (++autobaud_refused >= AUTOBAUD_REFUSED_MAX) {
			autobaud_refused = 0;
			LOG_WRN("UART autobaud: %u windows in a row disagreed "
				"(low=%u, high=%u), not touching the rate",
				AUTOBAUD_REFUSED_MAX, low, high);
		}
		return;
	}
	autobaud_refused = 0;

	div_meas = MIN(low, high);
	old_baud = linkr_uart_config_current();

	/* Both divisors are "source-clock cycles per bit", so the source clock
	 * cancels out: correct whether the UART runs off 80 MHz APB or 40 MHz
	 * XTAL, and no clock-tree call is needed. */
	div_cur = (hw->clk_div.div_int << 4) | hw->clk_div.div_frag;
	measured = ((uint64_t)old_baud * div_cur) / ((uint64_t)div_meas << 4);

	if (measured < AUTOBAUD_MIN_BAUD || measured > AUTOBAUD_MAX_BAUD) {
		autobaud_forget();
		if (now - autobaud_warned_at >= AUTOBAUD_WARN_MS) {
			autobaud_warned_at = now;
			LOG_WRN("UART autobaud: implausible %u cycles/bit -> "
				"%u, keeping %u",
				div_meas, (uint32_t)measured, old_baud);
		}
		return;
	}

	next = autobaud_snap((uint32_t)measured);
	lo = old_baud - old_baud / 100u * AUTOBAUD_TOLERANCE_PCT;
	hi = old_baud + old_baud / 100u * AUTOBAUD_TOLERANCE_PCT;

	if (measured >= lo && measured <= hi) {
		autobaud_forget(); /* we already match the line */
		return;
	}

	if (next != autobaud_candidate) {
		autobaud_candidate = next;
		autobaud_confirm = 1;
		return;
	}

	if (++autobaud_confirm < AUTOBAUD_CONFIRM) {
		return;
	}

	if (linkr_uart_config_reconfigure(next)) {
		autobaud_forget();
		LOG_WRN("UART autobaud: uart_configure(%u) rejected", next);
		return;
	}

	autobaud_forget();
	autobaud_cooldown_until = now + AUTOBAUD_COOLDOWN_MS;

	/* Reading the divider back is what proves the change reached the
	 * silicon: uart_configure() returning 0 only proves it was accepted.
	 * 694+7/16 = 115200, 8333+5/16 = 9600 at 80 MHz. */
	LOG_WRN("UART autobaud: line rate %u -> %u (min pulse %u cycles, "
		"low=%u high=%u), divider now %u + %u/16",
		old_baud, next, div_meas, low, high, hw->clk_div.div_int,
		hw->clk_div.div_frag);
}

void linkr_uart_autobaud_init(uintptr_t reg_addr)
{
	autobaud_reg = reg_addr;
	/* Relative to now, so the wrap-safe comparison in the tick stays
	 * valid however long the board has already been up. */
	autobaud_cooldown_until = k_uptime_get_32();
	/* Backdate the first warning too, so an implausible reading right
	 * after reset is reported rather than swallowed by the rate limit. */
	autobaud_warned_at = k_uptime_get_32() - AUTOBAUD_WARN_MS;

	k_work_init_delayable(&autobaud_work, autobaud_tick);

	/* Arm immediately: the peer's rate is known one window after reset
	 * rather than only after something has already gone wrong. */
	autobaud_arm(autobaud_hw());
	k_work_reschedule(&autobaud_work, K_MSEC(AUTOBAUD_WINDOW_MS));
}

#endif /* LINKR_UART_AUTOBAUD */
