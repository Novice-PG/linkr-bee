/*
 * NEC infrared remote codec.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "ir_nec.h"

#include <string.h>

#define NEC_BIT(n) (1u << (n))

enum nec_state {
	NEC_WAIT_LEADER_MARK = 0,
	NEC_WAIT_LEADER_SPACE,
	NEC_WAIT_BIT_MARK,
	NEC_WAIT_BIT_SPACE,
};

static bool in_range(uint32_t value, uint32_t low, uint32_t high)
{
	return value >= low && value <= high;
}

void linkr_nec_decoder_reset(struct linkr_nec_decoder *decoder)
{
	if (decoder == NULL) {
		return;
	}

	memset(decoder, 0, sizeof(*decoder));
	decoder->state = NEC_WAIT_LEADER_MARK;
}

/* Turn the 32 collected bits into a frame, or count a malformed payload. */
static enum linkr_nec_event decode_payload(struct linkr_nec_decoder *decoder,
					   struct linkr_nec_frame *frame)
{
	uint8_t address_low = (uint8_t)(decoder->payload & 0xff);
	uint8_t address_high = (uint8_t)((decoder->payload >> 8) & 0xff);
	uint8_t command = (uint8_t)((decoder->payload >> 16) & 0xff);
	uint8_t command_inv = (uint8_t)((decoder->payload >> 24) & 0xff);

	if (command_inv != (uint8_t)~command) {
		decoder->glitches++;
		return LINKR_NEC_EVENT_NONE;
	}

	if (address_high == (uint8_t)~address_low) {
		frame->address = address_low;
		frame->extended = false;
	} else {
		/* Extended NEC carries a full 16-bit address, no complement. */
		frame->address = (uint16_t)address_low | ((uint16_t)address_high << 8);
		frame->extended = true;
	}

	frame->command = command;
	frame->repeat = false;
	decoder->frames++;
	return LINKR_NEC_EVENT_FRAME;
}

enum linkr_nec_event linkr_nec_feed(struct linkr_nec_decoder *decoder, bool mark,
				    uint32_t duration_us,
				    struct linkr_nec_frame *frame)
{
	enum linkr_nec_event event = LINKR_NEC_EVENT_NONE;

	if (decoder == NULL || frame == NULL) {
		return LINKR_NEC_EVENT_NONE;
	}

	switch (decoder->state) {
	case NEC_WAIT_LEADER_MARK:
		if (!mark) {
			/* A gap after a completed frame lands here; ignore it. */
			return LINKR_NEC_EVENT_NONE;
		}
		if (in_range(duration_us, LINKR_NEC_LEADER_MARK_MIN_US,
			     LINKR_NEC_LEADER_MARK_MAX_US)) {
			decoder->state = NEC_WAIT_LEADER_SPACE;
			return LINKR_NEC_EVENT_NONE;
		}
		if (duration_us <= LINKR_NEC_TRAILING_MARK_MAX_US) {
			/* The stop mark of the frame we just published. */
			return LINKR_NEC_EVENT_NONE;
		}
		decoder->glitches++;
		return LINKR_NEC_EVENT_NONE;

	case NEC_WAIT_LEADER_SPACE:
		if (mark) {
			decoder->glitches++;
			decoder->state = NEC_WAIT_LEADER_MARK;
			return LINKR_NEC_EVENT_NONE;
		}
		if (in_range(duration_us, LINKR_NEC_LEADER_SPACE_MIN_US,
			     LINKR_NEC_LEADER_SPACE_MAX_US)) {
			decoder->bit_count = 0;
			decoder->payload = 0;
			decoder->state = NEC_WAIT_BIT_MARK;
			return LINKR_NEC_EVENT_NONE;
		}
		if (in_range(duration_us, LINKR_NEC_REPEAT_SPACE_MIN_US,
			     LINKR_NEC_REPEAT_SPACE_MAX_US)) {
			decoder->repeats++;
			memset(frame, 0, sizeof(*frame));
			frame->repeat = true;
			event = LINKR_NEC_EVENT_REPEAT;
		} else {
			decoder->glitches++;
		}
		decoder->state = NEC_WAIT_LEADER_MARK;
		return event;

	case NEC_WAIT_BIT_MARK:
		if (!mark) {
			decoder->glitches++;
			decoder->state = NEC_WAIT_LEADER_MARK;
			return LINKR_NEC_EVENT_NONE;
		}
		if (in_range(duration_us, LINKR_NEC_BIT_MARK_MIN_US,
			     LINKR_NEC_BIT_MARK_MAX_US)) {
			decoder->state = NEC_WAIT_BIT_SPACE;
			return LINKR_NEC_EVENT_NONE;
		}
		/* A leader-length mark means the previous frame was cut short. */
		if (in_range(duration_us, LINKR_NEC_LEADER_MARK_MIN_US,
			     LINKR_NEC_LEADER_MARK_MAX_US)) {
			decoder->glitches++;
			decoder->state = NEC_WAIT_LEADER_SPACE;
			return LINKR_NEC_EVENT_NONE;
		}
		decoder->glitches++;
		decoder->state = NEC_WAIT_LEADER_MARK;
		return LINKR_NEC_EVENT_NONE;

	case NEC_WAIT_BIT_SPACE:
	default:
		if (mark) {
			decoder->glitches++;
			decoder->state = NEC_WAIT_LEADER_MARK;
			return LINKR_NEC_EVENT_NONE;
		}
		if (duration_us <= LINKR_NEC_ZERO_SPACE_MAX_US) {
			decoder->payload &= ~NEC_BIT(decoder->bit_count);
		} else if (duration_us <= LINKR_NEC_ONE_SPACE_MAX_US) {
			decoder->payload |= NEC_BIT(decoder->bit_count);
		} else {
			/* Truncated frame: the line went idle mid-payload. */
			decoder->glitches++;
			decoder->state = NEC_WAIT_LEADER_MARK;
			return LINKR_NEC_EVENT_NONE;
		}

		decoder->bit_count++;
		if (decoder->bit_count >= LINKR_NEC_BITS) {
			decoder->state = NEC_WAIT_LEADER_MARK;
			decoder->bit_count = 0;
			return decode_payload(decoder, frame);
		}
		decoder->state = NEC_WAIT_BIT_MARK;
		return LINKR_NEC_EVENT_NONE;
	}
}

size_t linkr_nec_encode(uint16_t address, uint8_t command, uint32_t *durations,
			size_t capacity)
{
	uint32_t payload;
	size_t count = 0;

	if (durations == NULL || capacity < LINKR_NEC_MAX_DURATIONS) {
		return 0;
	}

	if (address <= 0xffu) {
		payload = (uint32_t)address | ((uint32_t)(uint8_t)~address << 8);
	} else {
		payload = (uint32_t)(address & 0xff) | ((uint32_t)(address >> 8) << 8);
	}
	payload |= (uint32_t)command << 16;
	payload |= (uint32_t)(uint8_t)~command << 24;

	durations[count++] = LINKR_NEC_LEADER_MARK_US;
	durations[count++] = LINKR_NEC_LEADER_SPACE_US;

	for (uint32_t bit = 0; bit < LINKR_NEC_BITS; bit++) {
		durations[count++] = LINKR_NEC_BIT_MARK_US;
		durations[count++] = (payload & NEC_BIT(bit)) ? LINKR_NEC_ONE_SPACE_US
							  : LINKR_NEC_ZERO_SPACE_US;
	}

	durations[count++] = LINKR_NEC_BIT_MARK_US;
	durations[count++] = LINKR_NEC_FRAME_GAP_US;
	return count;
}
