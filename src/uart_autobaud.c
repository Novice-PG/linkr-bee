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
 * blocks a later answer. (A stream of 0x00 or 0xFF is not empty — the start
 * and stop bits keep toggling the line, so it also keeps resetting the silence
 * counter — but it does not measure either: the two polarities time runs of
 * nine and one bits, fail autobaud_apart() in every window, and are discarded
 * like any window that cannot be trusted. Nothing is taken from it, and
 * nothing needs to be.)
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
 *
 * What 2% cannot catch is a line that reads *low* in every window forever.
 * A framed byte holds the line low for at most its start bit plus eight zero
 * data bits — the stop bit is always high and cuts it short — so a low run is
 * never longer than nine bit-times, and the two polarities only reach the
 * decision together when they timed the same run. A window with no
 * single-bit run anywhere in it therefore reports our own rate divided by
 * some k. That divisor is the shortest run the window left, and a byte with
 * no single-bit run can leave no run shorter than two: the start bit's own
 * low run is cut by the first data bit, so the low side is at least
 * start-plus-leading-zeros and the high side at least trailing-ones-plus-
 * stop, and the longest the two reach together is five bit-times (0xF0:
 * five low, five high). Two to five, then, is what a stream like this reads
 * as, with nothing to tell it from a peer that really slowed down. Measured: 0xCC at 115200 (runs of two and three bits) computes
 * 115200 x 11111 / (1389 x 16) = 57596, snaps to 57600, and agrees with
 * itself across windows — so "two windows agree" proves only that the pattern
 * is stable, not that the rate moved. Reconfiguring on it reaches 57600 by
 * *guessing*, and the line never said 57600: the pulse width belongs to RXD,
 * not to our divisor, so the port then reads 57600 forever and stays wrong.
 * That is a working link broken by a binary file.
 *
 * So the port keeps a memory of having been measured against the line, and it
 * starts out remembering: the rate in Kconfig is what both ends were set to,
 * so a reading that disagrees from the very first window is the one that has
 * to explain itself. Every reading within 2% of our own marks the link as
 * verified again, and while it is marked a reading that is our own rate
 * divided by k (2..9) is refused and logged instead of obeyed — it is the one
 * shape a missing single-bit run can produce, and the one direction it can
 * produce it in. Readings at or above our rate still move the port, because no
 * absent single-bit run can ever read high: that is the direction the
 * Kconfig's 1500000 / 921600 / 1000000 SBC consoles come from, and it stays
 * open. Verification is dropped by every reconfiguration, so a guess that
 * turns out wrong can still be undone — the rollback the two-window rule
 * depends on.
 *
 * Measured on a real link, that rollback has one hole, and it is the small
 * delta. A 115200 line read 118342 — 2.7% high, because one window's shortest
 * pulse was cut a little short — in window after window, and the two-window
 * rule followed the port there; the move cost a ring reset (33 bytes gone
 * mid-transfer) and mis-sampling at the new rate cost ~3.5% of every byte
 * after, and the mis-sampling is what kept re-measuring the port as itself,
 * so "the next windows measure the true rate again" never happened. The hole
 * is that a reading a few percent off our own is indistinguishable from our
 * own rate read badly, and the rule this file states — hold the port that
 * works — was never applied to it: only the divisor shape was refused. A
 * reading within AUTOBAUD_HOLD_PCT of our own, taken while the port sits at a
 * rate worth trusting, is now held instead of chased.
 *
 * The hole is deeper than the percentage, though, because the detector's error
 * is a fixed number of *cycles* and not a fixed fraction of a bit-time.
 * Measured on hardware: a true 694-cycle pulse latched as 676, and a true
 * 699-cycle one as 675 — the same ~18 cycles short in both, at two different
 * port rates. A constant 18 cycles is 2.7% of a bit-time at 115200 but 5.3% at
 * 230400 and 21% at 921600, so a hold measured only in percent stops covering
 * the error exactly where the link gets fast. The hold therefore has two
 * mouths — the percentage band for the rate, and a cycle band for the raw
 * pulse count — and either one holds the port. See AUTOBAUD_HOLD_CYCLES for
 * the trade the second one makes.
 *
 * The hold is a bound on the *gap*, not a list of blessed rates: nothing a
 * peer does lands inside it. Every other rate a console boots to is at least
 * 20% away or a whole divisor — 115200 -> 230400 is +100%, 921600 -> 1000000
 * is +8.5% — and a rate that is odd but real, 150000 say, is far enough from
 * ours that it is followed exactly as it always was. Being a rate the snap
 * table knows is neither necessary nor sufficient for the port to move.
 *
 * What the hold does need is a port worth trusting: one the line has been
 * measured against, or the standard rate both ends were configured to. A port
 * at a rate some earlier decision moved to, with nothing measured since, gets
 * no hold — the first clean reading still names the real rate and puts it
 * back, which is the rollback the two-window rule always promised.
 *
 * Silence is how the refusal reopens — but only for the divisors a stream
 * cannot fake. A peer that really did slow to a whole divisor of our own and
 * a stream with no single-bit run in it arrive as the same number (921600 ->
 * 115200 and 0xCC on a 115200 line are both "current / k"), so while the link
 * is carrying traffic no reading tells them apart, and holding the port that
 * works is the cheaper of the mistakes. A line quiet for long enough is a
 * link we no longer hold any evidence about, and a divisor of six or more —
 * one no byte stream can leave, since five bit-times is the longest run both
 * polarities of a framed byte can time together — is then believed, which is
 * what makes 921600 -> 115200 reachable. A divisor inside two to five is
 * refused in every state, silence or not: that is exactly what an idle line
 * followed by a binary file reads as, and following it would take the port to
 * a rate the line never carried with no later window left to undo it. The
 * refusal is a delay for the divisors only the line can produce, and a hold
 * for the ones a stream produces too.
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
 * Consecutive windows in which the line did not toggle at all, after which the
 * port stops being treated as measured against it: 8 x 400 ms = 3.2 s of
 * silence. Not one window — the peer's bursts are ~200 ms apart, so a single
 * empty window can come back empty on timing alone and would drop the
 * protection between two bursts of the same link. Sustained quiet is the one
 * thing that can happen *before* a peer's rate changes, which is what makes it
 * the right signal to reopen on: long enough that a burst gap cannot reach it,
 * short enough to cover the gap a rate change on the other end leaves.
 */
#define AUTOBAUD_QUIET_UNVERIFY 8
/*
 * The refusal's bound while the link is trusted: every divisor from two to
 * nine is refused, whether or not a byte stream could have produced it. Only
 * the two-to-five half is a stream's doing (see AUTOBAUD_ARTIFACT_MAX_FACTOR
 * and the file header); the rest is refused because a busy line that suddenly
 * reads current/8 is a stutter, not a peer.
 */
#define AUTOBAUD_MAX_FACTOR	9
/*
 * The longest shortest-run a window with no single-bit run in it can leave on
 * both polarities at once, and so the largest divisor a byte stream can fake:
 * five (0xF0 times five low and five high). A divisor at or below this is
 * refused in every state, silence included — see the file header.
 */
#define AUTOBAUD_ARTIFACT_MAX_FACTOR 5
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
/*
 * How close to our own rate a reading may sit and still be treated as our own
 * rate measured badly rather than a rate worth chasing. Five percent: wide
 * enough to swallow the 2.7% a mis-measured window produced on real hardware,
 * narrow enough that every other rate a console boots to stays out of it —
 * the closest pair in the snap table is 921600 -> 1000000 at 8.5%, and every
 * divisor pair is further apart still. See the file header: this is a bound
 * on the gap, not a list of rates the port is allowed to move to.
 */
#define AUTOBAUD_HOLD_PCT	5
/*
 * The same hold expressed in the units the detector actually reports. Its error
 * is a fixed number of source-clock cycles rather than a fraction of a
 * bit-time — measured: 676 latched against a true 694 at 115200, and 675
 * against 699 at 118517, the same ~18 short in both. Eighteen cycles is 2.7% of
 * a bit-time at 115200 but 5.3% at 230400, 10.6% at 460800 and 21% at 921600,
 * so the percentage band above loses its grip on the error exactly where the
 * rate climbs. This band keeps it: a latched pulse within this many cycles of
 * the one the port is programmed for is the detector's own error, whatever rate
 * it happens to be wearing.
 *
 * Twenty-four cycles: comfortably wider than the 18-19 measured, and still far
 * narrower than every move between two rates a console boots to *below* ours —
 * the nearest of those, 115200 arriving on a 921600 port, is 607 cycles away.
 * The one thing it swallows is a small upward step at a high rate: 921600 ->
 * 1000000 is 7 cycles apart. That trade falls the same way the rest of this
 * file does. At 921600 the detector's own error is 18 cycles against a 7-cycle
 * step, so a port that followed that reading would land further from the peer
 * than one that held still did — the hold is not what makes that rate hard, the
 * detector is, and the hold is what keeps the port from chasing it.
 */
#define AUTOBAUD_HOLD_CYCLES	24
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
/* Windows in a row with nothing to time; see AUTOBAUD_QUIET_UNVERIFY. */
static uint8_t autobaud_quiet;
/*
 * The line has been read within 2% of what the port is set to, so we know the
 * two agree. Set by a match, dropped by every reconfiguration — a port we
 * just changed has not been measured at its new rate yet, and a guess that
 * came out wrong has to stay correctable.
 */
static bool autobaud_verified;

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

/*
 * The standard rate `measured` sits within 1% of, or 0 when it sits within 1%
 * of none — which is a rate no peer can be set to.
 */
static uint32_t autobaud_standard_within_1pct(uint32_t measured)
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
	return 0;
}

/* Rounds to a rate worth reporting when the measurement is a whisker off. */
static uint32_t autobaud_snap(uint32_t measured)
{
	uint32_t s = autobaud_standard_within_1pct(measured);

	return s ? s : measured;
}

/*
 * True when `rate` sits within the snap margin (1%) of a rate a console can
 * actually be set to — this is about the *port's* rate being one worth
 * trusting, not about which readings may move it. The rate in Kconfig is a
 * standard console rate, so a port still at (or snapped to) one is a port
 * both ends were configured to; the hold in the tick leans on that, because
 * it is only safe to call a near-our-rate reading "our own rate, badly
 * measured" while our own rate is one the line could plausibly be at. See
 * the file header — no reading is refused for being non-standard.
 */
static bool autobaud_is_standard(uint32_t rate)
{
	return autobaud_standard_within_1pct(rate) != 0;
}

/* True when `rate` sits within `pct` of `base`, either side. */
static bool autobaud_within_pct(uint32_t rate, uint32_t base, uint32_t pct)
{
	uint32_t delta = rate > base ? rate - base : base - rate;

	return delta * 100u <= base * pct;
}

/*
 * True when `div_meas` — the shortest pulse the detector latched, in
 * source-clock cycles per bit — sits within `cycles` of the pulse the port is
 * programmed for. `div_cur` is the same quantity scaled by sixteen
 * (`div_int << 4 | div_frag`), so it is rounded back down here with the same
 * rounding the divider arithmetic itself uses, and the two numbers below are
 * directly comparable. See AUTOBAUD_HOLD_CYCLES.
 */
static bool autobaud_pulse_within_cycles(uint32_t div_meas, uint32_t div_cur,
					 uint32_t cycles)
{
	uint32_t expect = (div_cur + 8u) >> 4;
	uint32_t delta = div_meas > expect ? div_meas - expect : expect - div_meas;

	return delta <= cycles;
}

/*
 * True when `rate` is our own rate seen through a window whose shortest run
 * was not one bit long — that is, current / k for some k in 2..max_factor,
 * inside the tolerance the port itself is held to.
 *
 * A reading below ours *has* to come from a missing single-bit run: that is
 * the only thing that makes a matching line read low, and this is the shape it
 * reads low in. max_factor decides how far to disbelieve it — the whole range
 * while the link is trusted, only the two-to-five a stream can reach once the
 * silence has handed the larger divisors back. A reading that is none of these
 * is not something a missing run can produce, so if it repeats the line really
 * did move.
 */
static bool autobaud_reads_like_a_missing_bit(uint32_t rate, uint32_t current,
					      uint32_t max_factor)
{
	for (uint32_t k = 2; k <= max_factor; k++) {
		uint32_t expect = current / k;
		uint32_t delta = rate > expect ? rate - expect : expect - rate;

		if (expect != 0 && delta * 100u <= expect * AUTOBAUD_TOLERANCE_PCT) {
			return true;
		}
	}
	return false;
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
		 * rate — so it must not eat the refusal budget either. It does
		 * say something about the *link*: a line this quiet is no
		 * longer one we hold any evidence about, so the port's rate
		 * stops being treated as measured against it and the next
		 * reading is followed again. See AUTOBAUD_QUIET_UNVERIFY. */
		if (++autobaud_quiet >= AUTOBAUD_QUIET_UNVERIFY) {
			autobaud_quiet = AUTOBAUD_QUIET_UNVERIFY;
			if (autobaud_verified) {
				autobaud_verified = false;
				LOG_WRN("UART autobaud: line silent for %u "
					"windows, no longer measured against "
					"the port",
					AUTOBAUD_QUIET_UNVERIFY);
			}
		}
		return;
	}
	autobaud_quiet = 0;

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
		/*
		 * The line and the port agree. That is the reading a link which
		 * is working hands back, and it is the evidence that makes the
		 * readings *below* ours worth disbelieving afterwards.
		 */
		autobaud_verified = true;
		autobaud_forget(); /* we already match the line */
		return;
	}

	/*
	 * Close to our own rate, and the port is one worth trusting: this is
	 * our own rate measured through a window whose shortest pulse was cut
	 * a little short, not a rate worth chasing. Chasing it is what cost a
	 * real board its data — the port was moved 2.7% for a reading that
	 * said nothing, the move reset the ring (33 bytes gone mid-transfer)
	 * and mis-sampling at the new rate cost ~3.5% of every byte after it.
	 *
	 * Note what this is *not*: a list of rates the port may move to. A
	 * peer set to an odd but real rate (150000, say) is outside the band
	 * and is followed as it always was; the hold is a bound on how small a
	 * move is worth making, and below a few percent the two rates work
	 * identically anyway.
	 *
	 * Two ways in, because the detector's error is fixed in cycles while
	 * the percentage band is fixed in percent, and the two only agree at
	 * 115200: see AUTOBAUD_HOLD_CYCLES. The pulse comparison also puts the
	 * numbers it decided on into the log, which is what the next person to
	 * look at this will need.
	 *
	 * The port has to be worth trusting for the hold to apply: measured
	 * against the line already, or the standard rate both ends were
	 * configured to. A port at a rate some earlier decision moved to, with
	 * nothing measured since, keeps no such claim — a clean reading of the
	 * real rate is then free to put it back.
	 */
	if ((autobaud_verified || autobaud_is_standard(old_baud)) &&
	    (autobaud_within_pct(measured, old_baud, AUTOBAUD_HOLD_PCT) ||
	     autobaud_pulse_within_cycles(div_meas, div_cur,
					  AUTOBAUD_HOLD_CYCLES))) {
		autobaud_forget();
		if (now - autobaud_warned_at >= AUTOBAUD_WARN_MS) {
			autobaud_warned_at = now;
			LOG_WRN("UART autobaud: line reads %u (min pulse %u "
				"cycles, port programmed for %u) — our own "
				"rate measured short, not a move; holding %u",
				(uint32_t)measured, div_meas,
				(uint32_t)((div_cur + 8u) >> 4), old_baud);
		}
		return;
	}

	/*
	 * Low by a whole factor of ourselves: the shape of a stream with no
	 * single-bit run in it, not of a peer that moved — see the file header
	 * for the measurement behind that. Every divisor a byte stream can
	 * reach (two to five) is refused whatever the link's state, because
	 * following one would take the port to a rate the line never carried
	 * and it could not be put back: the pulse width that produced this
	 * number belongs to RXD rather than to our divisor, so the port would
	 * read the same wrong answer in every window after the change and
	 * nothing would ever disagree with it again. While the link is still
	 * trusted the larger divisors are refused too — a busy line that
	 * suddenly reads current/8 is a stutter, not a peer — and it is the
	 * silence that hands those back.
	 */
	if (autobaud_reads_like_a_missing_bit(
		    next, old_baud,
		    autobaud_verified ? AUTOBAUD_MAX_FACTOR
				      : AUTOBAUD_ARTIFACT_MAX_FACTOR)) {
		autobaud_forget();
		if (now - autobaud_warned_at >= AUTOBAUD_WARN_MS) {
			autobaud_warned_at = now;
			LOG_WRN("UART autobaud: line reads %u every window while "
				"the port is %u — that is our own rate over a run "
				"longer than one bit, holding %u",
				next, old_baud, old_baud);
		}
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
	/* Nothing has measured the port at the rate it just moved to, so a
	 * wrong guess stays correctable — see autobaud_verified. */
	autobaud_verified = false;
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
	/*
	 * The configured rate is the working assumption: both ends were set to
	 * it, so the first reading that disagrees is the one that has to
	 * explain itself. Starting out *un*verified instead made the very first
	 * window free to move the port on a reading of our own rate over a run
	 * longer than one bit — 0xCC arriving from reset computes 57600 from a
	 * 115200 line, moves the port there, and then reads as a match at the
	 * rate it just moved to, so no later window ever disagrees with the
	 * guess and the link stays broken. Downward is still the one direction
	 * a believed port refuses; a reading that is none of current / k is
	 * followed here exactly as it always was.
	 */
	autobaud_verified = true;
	autobaud_quiet = 0;
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
