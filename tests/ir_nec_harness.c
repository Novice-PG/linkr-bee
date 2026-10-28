/* Host-side checks for the NEC codec. Compiles src/ir_nec.c unchanged: the
 * codec has no Zephyr dependencies, which is exactly why it is testable here.
 * No hardware is emulated beyond the mark/space durations the IR receiver
 * interrupt would hand the decoder. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "ir_nec.h"

/* Feed one encoded frame's durations, alternating mark and space. */
static void feed_durations(struct linkr_nec_decoder *decoder,
			   const uint32_t *durations, size_t count,
			   struct linkr_nec_frame *frame, unsigned int *frames,
			   unsigned int *repeats)
{
	for (size_t i = 0; i < count; i++) {
		enum linkr_nec_event event =
			linkr_nec_feed(decoder, (i % 2u) == 0u, durations[i], frame);

		if (event == LINKR_NEC_EVENT_FRAME) {
			(*frames)++;
		} else if (event == LINKR_NEC_EVENT_REPEAT) {
			(*repeats)++;
		}
	}
}

/* Same durations scaled in time, to prove the classifier tolerates a real
 * receiver's edge placement. */
static void scale_durations(uint32_t *durations, size_t count, uint32_t num,
			    uint32_t den)
{
	for (size_t i = 0; i < count; i++) {
		durations[i] = durations[i] * num / den;
	}
}

static void test_encode_layout(void)
{
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	size_t count = linkr_nec_encode(0x04, 0x08, durations,
					sizeof(durations) / sizeof(durations[0]));

	assert(count == LINKR_NEC_MAX_DURATIONS);
	assert(durations[0] == LINKR_NEC_LEADER_MARK_US);
	assert(durations[1] == LINKR_NEC_LEADER_SPACE_US);
	assert(durations[count - 2] == LINKR_NEC_BIT_MARK_US);
	assert(durations[count - 1] == LINKR_NEC_FRAME_GAP_US);

	/* Frames carry the 32 payload bits LSB first: 0x04, its complement,
	 * 0x08, its complement. Every cell must encode the matching bit. */
	{
		const uint32_t payload = 0x04u | (0xfbu << 8) | (0x08u << 16) |
					 (0xf7u << 24);

		for (size_t bit = 0; bit < LINKR_NEC_BITS; bit++) {
			uint32_t expected = (payload & (1u << bit))
						    ? LINKR_NEC_ONE_SPACE_US
						    : LINKR_NEC_ZERO_SPACE_US;

			assert(durations[2 + (2 * bit) + 1] == expected);
		}
	}
}

static void test_roundtrip_standard(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	unsigned int frames = 0, repeats = 0;
	size_t count;

	linkr_nec_decoder_reset(&decoder);
	count = linkr_nec_encode(0x04, 0x08, durations,
				 sizeof(durations) / sizeof(durations[0]));
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);

	assert(frames == 1);
	assert(repeats == 0);
	assert(decoder.glitches == 0);
	assert(frame.repeat == false);
	assert(frame.extended == false);
	assert(frame.address == 0x04);
	assert(frame.command == 0x08);
}

static void test_roundtrip_extended_address(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	unsigned int frames = 0, repeats = 0;
	size_t count;

	linkr_nec_decoder_reset(&decoder);
	count = linkr_nec_encode(0x1234, 0x56, durations,
				 sizeof(durations) / sizeof(durations[0]));
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);

	assert(frames == 1);
	assert(decoder.glitches == 0);
	assert(frame.extended == true);
	assert(frame.address == 0x1234);
	assert(frame.command == 0x56);
}

static void test_repeat_frame(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	unsigned int frames = 0, repeats = 0;
	const uint32_t repeat[] = {
		LINKR_NEC_LEADER_MARK_US,
		LINKR_NEC_REPEAT_SPACE_US,
		LINKR_NEC_BIT_MARK_US,
		LINKR_NEC_FRAME_GAP_US,
	};

	linkr_nec_decoder_reset(&decoder);
	feed_durations(&decoder, repeat, sizeof(repeat) / sizeof(repeat[0]),
		       &frame, &frames, &repeats);

	assert(frames == 0);
	assert(repeats == 1);
	/* The repeat frame's own stop mark and gap must not count as errors. */
	assert(decoder.glitches == 0);
}

static void test_tolerates_slow_and_fast_receivers(void)
{
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	/* 80 %, 90 % and 110 % of nominal: the classifier windows must hold
	 * for a real receiver whose edges are not exactly on 562 us. */
	const uint32_t num[] = {8, 9, 11};
	const uint32_t den[] = {10, 10, 10};
	size_t count = linkr_nec_encode(0x00, 0x55, durations,
					sizeof(durations) / sizeof(durations[0]));

	for (size_t i = 0; i < sizeof(num) / sizeof(num[0]); i++) {
		struct linkr_nec_decoder decoder;
		struct linkr_nec_frame frame;
		uint32_t scaled[LINKR_NEC_MAX_DURATIONS];
		unsigned int frames = 0, repeats = 0;

		memcpy(scaled, durations, sizeof(scaled));
		scale_durations(scaled, count, num[i], den[i]);
		linkr_nec_decoder_reset(&decoder);
		feed_durations(&decoder, scaled, count, &frame, &frames, &repeats);
		assert(frames == 1);
		assert(frame.address == 0x00);
		assert(frame.command == 0x55);
	}
}

/* Build a payload with a deliberately broken command complement. */
static size_t encode_raw_payload(uint32_t payload, uint32_t *durations)
{
	size_t count = 0;

	durations[count++] = LINKR_NEC_LEADER_MARK_US;
	durations[count++] = LINKR_NEC_LEADER_SPACE_US;
	for (size_t bit = 0; bit < LINKR_NEC_BITS; bit++) {
		durations[count++] = LINKR_NEC_BIT_MARK_US;
		durations[count++] = (payload & (1u << bit)) ? LINKR_NEC_ONE_SPACE_US
							     : LINKR_NEC_ZERO_SPACE_US;
	}
	durations[count++] = LINKR_NEC_BIT_MARK_US;
	durations[count++] = LINKR_NEC_FRAME_GAP_US;
	return count;
}

static void test_malformed_payload_is_rejected(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	unsigned int frames = 0, repeats = 0;
	/* address 0x04 / ~0x04, command 0x08 but command complement 0x00. */
	uint32_t payload = 0x04u | (0xfbu << 8) | (0x08u << 16);
	size_t count = encode_raw_payload(payload, durations);

	linkr_nec_decoder_reset(&decoder);
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);

	assert(frames == 0);
	assert(decoder.glitches == 1);
}

static void test_truncated_frame_recovers(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	unsigned int frames = 0, repeats = 0;
	size_t count;
	uint32_t payload = 0x04u | (0xfbu << 8) | (0x08u << 16) | (0xf7u << 24);

	linkr_nec_decoder_reset(&decoder);

	/* Leader plus five bit pairs, then the line goes idle. */
	durations[0] = LINKR_NEC_LEADER_MARK_US;
	durations[1] = LINKR_NEC_LEADER_SPACE_US;
	for (size_t bit = 0; bit < 5; bit++) {
		durations[2 + (2 * bit)] = LINKR_NEC_BIT_MARK_US;
		durations[2 + (2 * bit) + 1] =
			(payload & (1u << bit)) ? LINKR_NEC_ONE_SPACE_US
						: LINKR_NEC_ZERO_SPACE_US;
	}
	durations[12] = LINKR_NEC_BIT_MARK_US;
	durations[13] = LINKR_NEC_FRAME_GAP_US;
	feed_durations(&decoder, durations, 14, &frame, &frames, &repeats);

	assert(frames == 0);
	assert(decoder.glitches >= 1);

	/* The decoder must resynchronise on the next complete frame. */
	count = linkr_nec_encode(0x10, 0x20, durations,
				 sizeof(durations) / sizeof(durations[0]));
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);
	assert(frames == 1);
	assert(frame.address == 0x10);
	assert(frame.command == 0x20);
}

static void test_frame_boundaries_hold_across_two_frames(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	unsigned int frames = 0, repeats = 0;
	size_t count;

	linkr_nec_decoder_reset(&decoder);

	count = linkr_nec_encode(0x01, 0x02, durations,
				 sizeof(durations) / sizeof(durations[0]));
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);
	count = linkr_nec_encode(0x03, 0x04, durations,
				 sizeof(durations) / sizeof(durations[0]));
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);

	assert(frames == 2);
	assert(decoder.glitches == 0);
	assert(frame.address == 0x03);
	assert(frame.command == 0x04);
}

static void test_decoder_survives_noise(void)
{
	struct linkr_nec_decoder decoder;
	struct linkr_nec_frame frame;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	unsigned int frames = 0, repeats = 0;
	size_t count;
	const uint32_t noise[] = {120, 2000, 640, 640, 3000, 90, 450};

	linkr_nec_decoder_reset(&decoder);
	for (size_t i = 0; i < sizeof(noise) / sizeof(noise[0]); i++) {
		(void)linkr_nec_feed(&decoder, (i % 2u) == 0u, noise[i], &frame);
	}

	count = linkr_nec_encode(0x22, 0x33, durations,
				 sizeof(durations) / sizeof(durations[0]));
	feed_durations(&decoder, durations, count, &frame, &frames, &repeats);
	assert(frames == 1);
	assert(frame.address == 0x22);
	assert(frame.command == 0x33);
}

int main(void)
{
	test_encode_layout();
	test_roundtrip_standard();
	test_roundtrip_extended_address();
	test_repeat_frame();
	test_tolerates_slow_and_fast_receivers();
	test_malformed_payload_is_rejected();
	test_truncated_frame_recovers();
	test_frame_boundaries_hold_across_two_frames();
	test_decoder_survives_noise();
	printf("ir_nec harness: ok\n");
	return 0;
}
