/*
 * IR learning: raw mark/space capture, replay and persistence format.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "ir_learn.h"

#include <string.h>

#define BLOB_FLAG_COMPLETE 0x01
#define BLOB_CHECKSUM_SEED 0x5a5au
#define US_MAX 65535u

static uint16_t blob_checksum(const uint8_t *data, size_t length)
{
	uint16_t sum = BLOB_CHECKSUM_SEED;

	for (size_t i = 0; i < length; i++) {
		sum = (uint16_t)(sum + data[i]);
	}
	return sum;
}

static void put_le16(uint8_t *out, uint16_t value)
{
	out[0] = (uint8_t)(value & 0xffu);
	out[1] = (uint8_t)((value >> 8) & 0xffu);
}

static uint16_t get_le16(const uint8_t *in)
{
	return (uint16_t)(in[0] | ((uint16_t)in[1] << 8));
}

void linkr_ir_learn_reset(struct linkr_ir_learn *learn)
{
	if (learn == NULL) {
		return;
	}

	memset(learn, 0, sizeof(*learn));
}

void linkr_ir_learn_begin(struct linkr_ir_learn *learn)
{
	linkr_ir_learn_reset(learn);
}

enum linkr_ir_learn_step linkr_ir_learn_feed(struct linkr_ir_learn *learn,
					     bool mark, uint32_t duration_us)
{
	/* The train must start with a mark: a space here means the capture was
	 * armed in the middle of a frame, so drop it. */
	if ((learn->count % 2u) == 0u && !mark) {
		return LINKR_IR_LEARN_STEP_IDLE;
	}
	if ((learn->count % 2u) != 0u && mark) {
		linkr_ir_learn_reset(learn);
		return LINKR_IR_LEARN_STEP_BROKEN;
	}

	if (learn->count >= LINKR_IR_LEARN_MAX_EDGES) {
		learn->complete = false;
		return LINKR_IR_LEARN_STEP_OVERFLOW;
	}

	/* A single interval longer than a uint16 would not survive storage. */
	if (duration_us > US_MAX) {
		duration_us = US_MAX;
	}

	learn->durations[learn->count++] = (uint16_t)duration_us;
	return LINKR_IR_LEARN_STEP_RUNNING;
}

enum linkr_ir_learn_step linkr_ir_learn_finish(struct linkr_ir_learn *learn)
{
	if (learn->count < LINKR_IR_LEARN_MIN_EDGES) {
		/* Not a key press, just noise before a real frame. */
		linkr_ir_learn_reset(learn);
		return LINKR_IR_LEARN_STEP_IDLE;
	}

	if ((learn->count % 2u) == 0u) {
		/* Ending on a space means the closing rise was never seen; drop it
		 * so a replay ends with the carrier off after a mark. */
		learn->count--;
	}
	if (learn->count < LINKR_IR_LEARN_MIN_EDGES) {
		linkr_ir_learn_reset(learn);
		return LINKR_IR_LEARN_STEP_IDLE;
	}

	learn->complete = true;
	return LINKR_IR_LEARN_STEP_READY;
}

bool linkr_ir_learn_valid(const struct linkr_ir_learn *learn)
{
	return learn != NULL && learn->complete &&
	       learn->count >= LINKR_IR_LEARN_MIN_EDGES &&
	       learn->count <= LINKR_IR_LEARN_MAX_EDGES;
}

uint32_t linkr_ir_learn_total_us(const struct linkr_ir_learn *learn)
{
	uint32_t total = 0;

	if (learn == NULL) {
		return 0;
	}

	for (uint16_t i = 0; i < learn->count; i++) {
		total += learn->durations[i];
	}
	return total;
}

size_t linkr_ir_learn_serialize(const struct linkr_ir_learn *learn, uint8_t *blob,
				size_t capacity)
{
	size_t length;

	if (!linkr_ir_learn_valid(learn) || blob == NULL) {
		return 0;
	}

	length = LINKR_IR_LEARN_BLOB_HEADER + (2u * learn->count) +
		 LINKR_IR_LEARN_BLOB_TRAILER;
	if (capacity < length) {
		return 0;
	}

	blob[0] = LINKR_IR_LEARN_BLOB_MAGIC0;
	blob[1] = LINKR_IR_LEARN_BLOB_MAGIC1;
	blob[2] = LINKR_IR_LEARN_BLOB_VERSION;
	blob[3] = BLOB_FLAG_COMPLETE;
	put_le16(&blob[4], learn->count);
	blob[6] = 0;
	blob[7] = 0;

	for (uint16_t i = 0; i < learn->count; i++) {
		put_le16(&blob[LINKR_IR_LEARN_BLOB_HEADER + (2u * i)],
			 learn->durations[i]);
	}

	put_le16(&blob[length - LINKR_IR_LEARN_BLOB_TRAILER],
		 blob_checksum(blob, length - LINKR_IR_LEARN_BLOB_TRAILER));
	return length;
}

bool linkr_ir_learn_header_edges(const uint8_t *header, size_t total_length,
				 uint16_t *edges)
{
	uint16_t count;

	if (header == NULL) {
		return false;
	}
	if (header[0] != LINKR_IR_LEARN_BLOB_MAGIC0 ||
	    header[1] != LINKR_IR_LEARN_BLOB_MAGIC1 ||
	    header[2] != LINKR_IR_LEARN_BLOB_VERSION) {
		return false;
	}
	if ((header[3] & BLOB_FLAG_COMPLETE) == 0u) {
		return false;
	}

	count = get_le16(&header[4]);
	if (count < LINKR_IR_LEARN_MIN_EDGES ||
	    count > LINKR_IR_LEARN_MAX_EDGES) {
		return false;
	}
	if (total_length != LINKR_IR_LEARN_BLOB_HEADER + (2u * count) +
				  LINKR_IR_LEARN_BLOB_TRAILER) {
		return false;
	}
	if (edges != NULL) {
		*edges = count;
	}
	return true;
}

int linkr_ir_learn_parse(struct linkr_ir_learn *learn, const uint8_t *blob,
			 size_t length)
{
	size_t expected;
	uint16_t count;
	uint16_t checksum;

	if (learn == NULL || blob == NULL ||
	    length < LINKR_IR_LEARN_BLOB_HEADER + LINKR_IR_LEARN_BLOB_TRAILER) {
		return -1;
	}
	if (blob[0] != LINKR_IR_LEARN_BLOB_MAGIC0 ||
	    blob[1] != LINKR_IR_LEARN_BLOB_MAGIC1 ||
	    blob[2] != LINKR_IR_LEARN_BLOB_VERSION ||
	    (blob[3] & BLOB_FLAG_COMPLETE) == 0u) {
		return -1;
	}

	count = get_le16(&blob[4]);
	if (count < LINKR_IR_LEARN_MIN_EDGES || count > LINKR_IR_LEARN_MAX_EDGES) {
		return -1;
	}

	expected = LINKR_IR_LEARN_BLOB_HEADER + (2u * count) +
		   LINKR_IR_LEARN_BLOB_TRAILER;
	if (length != expected) {
		return -1;
	}

	checksum = get_le16(&blob[length - LINKR_IR_LEARN_BLOB_TRAILER]);
	if (checksum != blob_checksum(blob, length - LINKR_IR_LEARN_BLOB_TRAILER)) {
		return -1;
	}

	linkr_ir_learn_reset(learn);
	for (uint16_t i = 0; i < count; i++) {
		learn->durations[i] =
			get_le16(&blob[LINKR_IR_LEARN_BLOB_HEADER + (2u * i)]);
	}
	learn->count = count;
	learn->complete = true;
	return 0;
}
