/* Host-side checks for the IR learning capture and storage format.
 * Compiles src/ir_learn.c unchanged: it is pure logic on purpose, so the
 * capture rules and the NVS blob layout can be tested without an IR receiver. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "ir_learn.h"
#include "ir_nec.h"

/* Feed a mark/space train starting with a mark, like the interrupt handler
 * does once it has seen the opening falling edge. */
static enum linkr_ir_learn_step feed_train(struct linkr_ir_learn *learn,
					   const uint32_t *durations,
					   size_t count, size_t *fed)
{
	enum linkr_ir_learn_step step = LINKR_IR_LEARN_STEP_IDLE;

	for (size_t i = 0; i < count; i++) {
		step = linkr_ir_learn_feed(learn, (i % 2u) == 0u, durations[i]);
		if (fed != NULL) {
			*fed = i + 1;
		}
		if (step == LINKR_IR_LEARN_STEP_OVERFLOW ||
		    step == LINKR_IR_LEARN_STEP_BROKEN) {
			break;
		}
	}
	return step;
}

static void test_capture_of_nec_frame(void)
{
	struct linkr_ir_learn learn;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	size_t count;
	size_t fed = 0;
	enum linkr_ir_learn_step step;

	/* The encoder ends with the inter-frame gap, which never produces an
	 * edge on the receiver: the capture sees everything before it. */
	count = linkr_nec_encode(0x04, 0x08, durations,
				 sizeof(durations) / sizeof(durations[0]));
	assert(count == LINKR_NEC_MAX_DURATIONS);

	linkr_ir_learn_begin(&learn);
	step = feed_train(&learn, durations, count - 1u, &fed);
	assert(step == LINKR_IR_LEARN_STEP_RUNNING);
	assert(fed == count - 1u);
	/* The train ends on the stop mark, so the count is odd. */
	assert((learn.count % 2u) == 1u);

	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_READY);
	assert(linkr_ir_learn_valid(&learn));
	assert(learn.count == count - 1u);
	for (size_t i = 0; i < learn.count; i++) {
		assert(learn.durations[i] == durations[i]);
	}
	assert(linkr_ir_learn_total_us(&learn) > 0);
}

static void test_leading_space_is_ignored(void)
{
	struct linkr_ir_learn learn;

	linkr_ir_learn_begin(&learn);
	/* Capture armed mid-frame: the first interval seen is a space. */
	assert(linkr_ir_learn_feed(&learn, false, 4500) ==
	       LINKR_IR_LEARN_STEP_IDLE);
	assert(learn.count == 0);

	/* The next rising edge starts a real mark and collection proceeds. */
	assert(linkr_ir_learn_feed(&learn, true, 9000) ==
	       LINKR_IR_LEARN_STEP_RUNNING);
	assert(learn.count == 1);
}

static void test_lost_edge_drops_the_capture(void)
{
	struct linkr_ir_learn learn;

	linkr_ir_learn_begin(&learn);
	assert(linkr_ir_learn_feed(&learn, true, 9000) ==
	       LINKR_IR_LEARN_STEP_RUNNING);
	/* Two marks in a row cannot happen without a lost edge. */
	assert(linkr_ir_learn_feed(&learn, true, 562) ==
	       LINKR_IR_LEARN_STEP_BROKEN);
	assert(learn.count == 0);
	assert(!linkr_ir_learn_valid(&learn));
}

static void test_noise_and_short_frames_are_rejected(void)
{
	struct linkr_ir_learn learn;
	const uint32_t short_train[] = {100, 200, 120, 200, 140};
	const uint32_t long_intervals[] = {9000u, 4500u, 70000u, 562u, 562u, 562u,
					   562u};

	linkr_ir_learn_begin(&learn);
	(void)feed_train(&learn, short_train, 5, NULL);
	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_IDLE);
	assert(!linkr_ir_learn_valid(&learn));

	/* Longer than a uint16 microsecond interval is clamped, not wrapped. */
	linkr_ir_learn_begin(&learn);
	(void)feed_train(&learn, long_intervals, 7, NULL);
	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_READY);
	assert(learn.durations[2] == 65535);
}

static void test_trailing_space_is_dropped(void)
{
	struct linkr_ir_learn learn;
	const uint32_t train[] = {9000u, 4500u, 562u, 1687u, 562u, 562u, 562u,
				  30000u};

	linkr_ir_learn_begin(&learn);
	(void)feed_train(&learn, train, 8, NULL);
	/* Ending on a space means the closing rise was never captured. */
	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_READY);
	assert((learn.count % 2u) == 1u);
	assert(learn.count == 7);
}

static void test_overflow_is_reported(void)
{
	static struct linkr_ir_learn learn;
	enum linkr_ir_learn_step step = LINKR_IR_LEARN_STEP_RUNNING;

	linkr_ir_learn_begin(&learn);
	for (size_t i = 0; i < LINKR_IR_LEARN_MAX_EDGES; i++) {
		step = linkr_ir_learn_feed(&learn, (i % 2u) == 0u, 500);
		assert(step == LINKR_IR_LEARN_STEP_RUNNING);
	}
	assert(learn.count == LINKR_IR_LEARN_MAX_EDGES);
	/* The next interval does not fit. */
	step = linkr_ir_learn_feed(&learn, (LINKR_IR_LEARN_MAX_EDGES % 2u) == 0u,
				   500);
	assert(step == LINKR_IR_LEARN_STEP_OVERFLOW);
}

static void test_header_edges(void)
{
	struct linkr_ir_learn learn;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	uint8_t blob[LINKR_IR_LEARN_BLOB_MAX];
	size_t length;
	uint16_t edges = 0;
	size_t count = linkr_nec_encode(0x00, 0x55, durations,
					sizeof(durations) / sizeof(durations[0]));

	linkr_ir_learn_begin(&learn);
	(void)feed_train(&learn, durations, count - 1u, NULL);
	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_READY);
	length = linkr_ir_learn_serialize(&learn, blob, sizeof(blob));
	assert(length > 0);

	/* The boot path only reads the fixed header: it must agree with the blob
	 * length and the recorded edge count. */
	assert(linkr_ir_learn_header_edges(blob, length, &edges));
	assert(edges == learn.count);
	assert(linkr_ir_learn_header_edges(blob, length, NULL));

	uint8_t bad[LINKR_IR_LEARN_BLOB_HEADER];
	memcpy(bad, blob, sizeof(bad));
	assert(!linkr_ir_learn_header_edges(NULL, length, &edges));
	assert(!linkr_ir_learn_header_edges(bad, length - 1u, &edges));
	assert(!linkr_ir_learn_header_edges(bad, length + 1u, &edges));

	uint8_t damaged[LINKR_IR_LEARN_BLOB_HEADER];
	memcpy(damaged, blob, sizeof(damaged));
	damaged[0] = 'X';
	assert(!linkr_ir_learn_header_edges(damaged, length, &edges));
	memcpy(damaged, blob, sizeof(damaged));
	damaged[2] = 0xff;
	assert(!linkr_ir_learn_header_edges(damaged, length, &edges));
	memcpy(damaged, blob, sizeof(damaged));
	damaged[3] = 0;
	assert(!linkr_ir_learn_header_edges(damaged, length, &edges));
	memcpy(damaged, blob, sizeof(damaged));
	damaged[4] = 0;
	damaged[5] = 0;
	assert(!linkr_ir_learn_header_edges(damaged, length, &edges));
	memcpy(damaged, blob, sizeof(damaged));
	damaged[4] = 0xff;
	damaged[5] = 0xff;
	assert(!linkr_ir_learn_header_edges(damaged, length, &edges));
}

static void test_serialize_roundtrip(void)
{
	struct linkr_ir_learn learn;
	struct linkr_ir_learn parsed;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	uint8_t blob[LINKR_IR_LEARN_BLOB_MAX];
	size_t count = linkr_nec_encode(0x00, 0x55, durations,
					sizeof(durations) / sizeof(durations[0]));
	size_t length;

	linkr_ir_learn_begin(&learn);
	(void)feed_train(&learn, durations, count - 1u, NULL);
	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_READY);

	length = linkr_ir_learn_serialize(&learn, blob, sizeof(blob));
	assert(length == LINKR_IR_LEARN_BLOB_HEADER + (2u * learn.count) +
				LINKR_IR_LEARN_BLOB_TRAILER);
	assert(blob[0] == 'L' && blob[1] == 'I');

	memset(&parsed, 0, sizeof(parsed));
	assert(linkr_ir_learn_parse(&parsed, blob, length) == 0);
	assert(parsed.count == learn.count);
	assert(parsed.complete);
	assert(memcmp(parsed.durations, learn.durations,
		      2u * learn.count) == 0);

	/* Too small a destination means "do not write", not a truncated blob. */
	assert(linkr_ir_learn_serialize(&learn, blob, length - 1u) == 0);
}

static void test_corrupt_blobs_are_rejected(void)
{
	struct linkr_ir_learn learn;
	struct linkr_ir_learn parsed;
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	uint8_t blob[LINKR_IR_LEARN_BLOB_MAX];
	uint8_t damaged[LINKR_IR_LEARN_BLOB_MAX];
	size_t count = linkr_nec_encode(0x01, 0x02, durations,
					sizeof(durations) / sizeof(durations[0]));
	size_t length;

	linkr_ir_learn_begin(&learn);
	(void)feed_train(&learn, durations, count - 1u, NULL);
	assert(linkr_ir_learn_finish(&learn) == LINKR_IR_LEARN_STEP_READY);
	length = linkr_ir_learn_serialize(&learn, blob, sizeof(blob));
	assert(length > 0);

	/* A flipped payload byte must fail the checksum. */
	memcpy(damaged, blob, length);
	damaged[LINKR_IR_LEARN_BLOB_HEADER + 1] ^= 0x40;
	assert(linkr_ir_learn_parse(&parsed, damaged, length) != 0);

	/* Wrong magic, wrong version, truncated and padded blobs are refused. */
	memcpy(damaged, blob, length);
	damaged[0] = 'X';
	assert(linkr_ir_learn_parse(&parsed, damaged, length) != 0);

	memcpy(damaged, blob, length);
	damaged[2] = 0x7f;
	assert(linkr_ir_learn_parse(&parsed, damaged, length) != 0);

	assert(linkr_ir_learn_parse(&parsed, blob, length - 1u) != 0);
	assert(linkr_ir_learn_parse(&parsed, blob, length + 1u) != 0);
	assert(linkr_ir_learn_parse(&parsed, blob, 4) != 0);
	assert(linkr_ir_learn_parse(&parsed, NULL, length) != 0);
}

int main(void)
{
	test_capture_of_nec_frame();
	test_leading_space_is_ignored();
	test_lost_edge_drops_the_capture();
	test_noise_and_short_frames_are_rejected();
	test_trailing_space_is_dropped();
	test_overflow_is_reported();
	test_header_edges();
	test_serialize_roundtrip();
	test_corrupt_blobs_are_rejected();
	printf("ir_learn harness: ok\n");
	return 0;
}
