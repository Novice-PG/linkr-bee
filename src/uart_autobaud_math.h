/*
 * The arithmetic behind adaptive baud detection, without the hardware.
 *
 * This header has no Zephyr, no ESP-IDF and no register definitions: it is a
 * few integer functions over <stdint.h>, so the part that can be wrong can be
 * compiled on a host and tested. Getting a rate wrong here reconfigures a UART
 * that was working, which loses every byte in flight and looks exactly like a
 * wireless fault from the operator's side.
 *
 * The whole design rests on one invariant. A properly framed UART line never
 * holds a level for less than one bit time, so if the hardware reports the
 * shortest pulse it saw as `min_cycles` source-clock cycles, then
 *
 *     min_cycles >= clock_hz / true_rate       hence
 *     clock_hz / min_cycles <= true_rate
 *
 * Every measurement is therefore a *lower bound* on the peer's real rate, and
 * it is exact as soon as any byte in the window carried an isolated bit. That
 * asymmetry is what makes "take the largest measurement seen" the right
 * reduction instead of an average: a window whose bytes happened to leave no
 * single-bit run reports the true rate divided by some whole k, and those
 * windows are silently beaten by any window that did time a single bit.
 */
#ifndef LINKR_UART_AUTOBAUD_MATH_H_
#define LINKR_UART_AUTOBAUD_MATH_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/** Below this no peer we support talks; it is the Kconfig range's floor. */
#define LINKR_AUTOBAUD_MIN_RATE 300u
/** The ESP32 UART's own ceiling, and far above any serial console. */
#define LINKR_AUTOBAUD_MAX_RATE 3000000u

/*
 * How far a measurement may sit from a standard rate and still be called that
 * rate. UART framing tolerates roughly 2-3% of mismatch before it starts to
 * slip a bit, so neighbouring entries must be further apart than this for the
 * answer to be unambiguous. The list below keeps every adjacent pair at least
 * 8.5% apart (the closest are 460800/500000 and 921600/1000000), which leaves
 * 3% plenty of margin on both sides.
 */
#define LINKR_AUTOBAUD_TOLERANCE_PCT 3u

/**
 * Whether two rates are within `pct` percent of each other.
 *
 * The comparison is against the larger of the two and symmetric, so "is 115070
 * within 3% of 115200" and "is 115200 within 3% of 115070" agree. Comparing
 * against one operand instead would make the answer depend on which side the
 * measurement landed, and a boundary that moves is a boundary you debug later.
 */
static inline bool linkr_autobaud_within(uint32_t a, uint32_t b, uint32_t pct)
{
	uint32_t higher = a > b ? a : b;
	uint32_t spread = higher - (a > b ? b : a);

	if (higher == 0) {
		return false;
	}

	/* 3% of three million still fits comfortably in 32 bits. */
	return spread * 100u <= higher * pct;
}

/**
 * How far the two polarities may disagree and still be counted as one window.
 *
 * Wider than the rate tolerance on purpose: a window whose lowest and highest
 * runs of one bit agree to 5% has seen the same event twice, while the rate
 * tolerance only has to keep a snapped answer unambiguous.
 */
#define LINKR_AUTOBAUD_AGREE_PCT 5u

/**
 * Whether the low and high latches timed the same shortest run.
 *
 * On a line carrying one-bit runs both polarities latch the same number of
 * source-clock cycles — measured on hardware as exactly equal (8319/8319,
 * 692/692, 4159/4159) — so a window in which they differ by more than the
 * agree margin did not time one event twice. The smaller reading is then a
 * spike that passed the glitch filter, or the half-pulse left when stty moved
 * the divisor while its own bytes were still queued (measured: 461/692 and
 * 1776/4159). One window cannot say which side is wrong.
 *
 * Acting on one anyway is how a port gets taken somewhere the line never was.
 * The rate reduction takes the smaller reading, so an 80-cycle spike on a
 * 694-cycle bit reads as 1 Mbaud, snaps cleanly to 1000000, and then holds:
 * every honest window that follows is slower than it, so those windows only
 * increment the count that says the best reading has been holding, and the
 * third one decides. The filter that should have caught the spike runs at 16
 * cycles, and this is what catches the rest.
 *
 * A window of 0x00 or 0xFF is refused by the same rule — its polarities time
 * nine and one bits — which costs that window and nothing else: a line
 * carrying no other byte carries no rate this rule could trust anyway.
 *
 * The margin is measured against the smaller reading, because both are lower
 * bounds on the true bit time and the smaller is therefore the closer of the
 * two.
 *
 * Two zeroes compare as agreeing: nothing was timed, and the caller already
 * treats that as silence. A zero against a real pulse does not — one polarity
 * seeing nothing is a window that only half happened.
 */
static inline bool linkr_autobaud_apart(uint32_t low, uint32_t high)
{
	uint32_t lo = low < high ? low : high;
	uint32_t hi = low < high ? high : low;

	return (hi - lo) * 100u > lo * LINKR_AUTOBAUD_AGREE_PCT;
}

/**
 * Turn the shortest pulse the hardware saw into a line rate, or 0 if the
 * reading cannot be one.
 *
 * @param min_cycles Shortest low or high run, in source-clock cycles.
 * @param clock_hz   The UART's source clock, e.g. 80 MHz APB.
 */
static inline uint32_t linkr_autobaud_rate(uint32_t min_cycles, uint32_t clock_hz)
{
	uint32_t rate;

	if (min_cycles == 0) {
		/* Nothing was timed: the latches are still at their reset value,
		 * which is not "a very fast line". */
		return 0;
	}

	rate = clock_hz / min_cycles;

	if (rate < LINKR_AUTOBAUD_MIN_RATE || rate > LINKR_AUTOBAUD_MAX_RATE) {
		/* A glitch that slipped past the filter, or a line that is being
		 * driven by something that is not a UART. Either way it is not a
		 * rate we could set the port to. */
		return 0;
	}

	return rate;
}

/**
 * Snap a measured rate to the nearest rate we are willing to program, or 0 if
 * none is close enough.
 *
 * Refusing to answer is the useful behaviour. A line running at some
 * arbitrary baud macOS invented cannot be served by moving to the nearest
 * standard rate — the mismatch would corrupt bytes just as surely as doing
 * nothing — and saying so is better than pretending.
 */
static inline uint32_t linkr_autobaud_snap(uint32_t rate)
{
	/*
	 * 14400 and 74880 are the two that are easy to leave out of a table
	 * built from what a terminal program offers. 14400 is a real standard
	 * rate sitting between 9600 and 19200; 74880 is the ESP32's own ROM
	 * loader rate, so a peer still in its bootloader announces itself there
	 * before anything has configured it, and refusing it would turn that
	 * announcement into noise. Neither can be confused with a neighbour:
	 * the closest new pair is 57600/74880, 30% apart.
	 */
	static const uint32_t rates[] = {
		300u,     600u,     1200u,    2400u,    4800u,
		9600u,    14400u,   19200u,   38400u,   57600u,
		74880u,   115200u,  230400u,  460800u,  500000u,
		921600u,  1000000u, 1500000u, 2000000u, 3000000u,
	};

	if (rate < LINKR_AUTOBAUD_MIN_RATE || rate > LINKR_AUTOBAUD_MAX_RATE) {
		return 0;
	}

	for (size_t i = 0; i < (sizeof(rates) / sizeof(rates[0])); i++) {
		if (linkr_autobaud_within(rates[i], rate,
					  LINKR_AUTOBAUD_TOLERANCE_PCT)) {
			return rates[i];
		}
	}

	return 0;
}

#endif /* LINKR_UART_AUTOBAUD_MATH_H_ */
