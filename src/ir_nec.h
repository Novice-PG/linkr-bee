/*
 * NEC infrared remote codec (pure functions, no Zephyr dependencies).
 *
 * The Linkr Bee X1.0 IR hardware hands the firmware a demodulated signal: a
 * mark (carrier present) reads as a low level, a space reads high. This file
 * turns a sequence of mark/space durations into frames and back, so the same
 * logic can be exercised by a host test and by the GPIO interrupt handler.
 *
 * NEC frame: 9 ms mark, 4.5 ms space, then 32 bits LSB first. Every bit is a
 * 562 us mark plus a 562 us (0) or 1687 us (1) space. A repeat frame is the
 * 9 ms mark plus a 2.25 ms space and a final 562 us mark.
 *
 * Standard NEC carries an 8-bit address and its complement; the extended
 * variant replaces the complement with the high address byte.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#ifndef LINKR_IR_NEC_H_
#define LINKR_IR_NEC_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define LINKR_NEC_LEADER_MARK_US 9000u
#define LINKR_NEC_LEADER_SPACE_US 4500u
#define LINKR_NEC_REPEAT_SPACE_US 2250u
#define LINKR_NEC_BIT_MARK_US 562u
#define LINKR_NEC_ZERO_SPACE_US 562u
#define LINKR_NEC_ONE_SPACE_US 1687u
#define LINKR_NEC_FRAME_GAP_US 40000u

#define LINKR_NEC_BITS 32u

/* One encoded frame: leader pair, 32 bit pairs, stop mark, tail gap. */
#define LINKR_NEC_MAX_DURATIONS (2u + (2u * LINKR_NEC_BITS) + 2u)

/* Receive classification windows (microseconds). */
#define LINKR_NEC_LEADER_MARK_MIN_US 7000u
#define LINKR_NEC_LEADER_MARK_MAX_US 11000u
#define LINKR_NEC_LEADER_SPACE_MIN_US 3500u
#define LINKR_NEC_LEADER_SPACE_MAX_US 5500u
#define LINKR_NEC_REPEAT_SPACE_MIN_US 1800u
#define LINKR_NEC_REPEAT_SPACE_MAX_US 2800u
#define LINKR_NEC_BIT_MARK_MIN_US 300u
#define LINKR_NEC_BIT_MARK_MAX_US 900u
#define LINKR_NEC_ZERO_SPACE_MAX_US 1000u
#define LINKR_NEC_ONE_SPACE_MAX_US 2400u

/* Marks shorter than this after a frame are the frame's own stop mark. */
#define LINKR_NEC_TRAILING_MARK_MAX_US 2000u

enum linkr_nec_event {
	LINKR_NEC_EVENT_NONE = 0,
	LINKR_NEC_EVENT_FRAME,
	LINKR_NEC_EVENT_REPEAT,
};

struct linkr_nec_frame {
	uint16_t address;
	uint8_t command;
	bool extended; /* true when the address is the 16-bit extended form */
	bool repeat;   /* true for a repeat frame (no address/command) */
};

struct linkr_nec_decoder {
	uint8_t state;
	uint8_t bit_count;
	uint32_t payload;
	uint32_t frames;
	uint32_t repeats;
	uint32_t glitches;
};

/* Reset the decoder state and its counters. */
void linkr_nec_decoder_reset(struct linkr_nec_decoder *decoder);

/* Feed one edge interval. `mark` is true when the interval was carrier on
 * (receiver output low). Returns what the interval completed, if anything. */
enum linkr_nec_event linkr_nec_feed(struct linkr_nec_decoder *decoder, bool mark,
				    uint32_t duration_us,
				    struct linkr_nec_frame *frame);

/* Encode a frame into alternating mark/space durations (microseconds).
 * Returns the number of entries, or 0 when `capacity` is too small. */
size_t linkr_nec_encode(uint16_t address, uint8_t command, uint32_t *durations,
			size_t capacity);

#endif /* LINKR_IR_NEC_H_ */
