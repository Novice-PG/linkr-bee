/*
 * IR learning: capture and replay an arbitrary remote-control frame.
 *
 * The NEC decoder only understands one protocol. "Learning" keeps the raw
 * mark/space train instead, exactly the way an IR remote's own chip does: the
 * receiver interrupt records every interval, silence closes the frame, and the
 * replay drives the same durations back out through the carrier gate. That
 * covers remotes that are not NEC (RC5, Sony, JVC, ...) as long as one key
 * press produces at most LINKR_IR_LEARN_MAX_EDGES intervals.
 *
 * This file is pure logic so a host test can drive it: no Zephyr types, no
 * hardware. The GPIO interrupt handler feeds intervals in, the command layer
 * stops the capture and decides where to store it.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#ifndef LINKR_IR_LEARN_H_
#define LINKR_IR_LEARN_H_

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_MAX_EDGES
#define LINKR_IR_LEARN_MAX_EDGES CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_MAX_EDGES
#else
#define LINKR_IR_LEARN_MAX_EDGES 192
#endif

#ifdef CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_SLOTS
#define LINKR_IR_LEARN_SLOTS CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_SLOTS
#else
#define LINKR_IR_LEARN_SLOTS 8
#endif

/* A frame shorter than this is noise, not a key press. */
#define LINKR_IR_LEARN_MIN_EDGES 6

/* Storage format: "LI" version flags count[2] reserved[2], durations, sum. */
#define LINKR_IR_LEARN_BLOB_MAGIC0 'L'
#define LINKR_IR_LEARN_BLOB_MAGIC1 'I'
#define LINKR_IR_LEARN_BLOB_VERSION 1
#define LINKR_IR_LEARN_BLOB_HEADER 8
#define LINKR_IR_LEARN_BLOB_TRAILER 2
#define LINKR_IR_LEARN_BLOB_MAX \
	(LINKR_IR_LEARN_BLOB_HEADER + (2 * LINKR_IR_LEARN_MAX_EDGES) + \
	 LINKR_IR_LEARN_BLOB_TRAILER)

enum linkr_ir_learn_step {
	LINKR_IR_LEARN_STEP_IDLE = 0, /* nothing captured yet */
	LINKR_IR_LEARN_STEP_RUNNING,  /* still collecting intervals */
	LINKR_IR_LEARN_STEP_READY,    /* a frame is complete */
	LINKR_IR_LEARN_STEP_OVERFLOW, /* more edges than the buffer holds */
	LINKR_IR_LEARN_STEP_BROKEN,   /* mark/space pairing was lost */
};

struct linkr_ir_learn {
	uint16_t count;
	bool complete;
	uint16_t durations[LINKR_IR_LEARN_MAX_EDGES];
};

/* Drop any capture, ready to start another one. */
void linkr_ir_learn_reset(struct linkr_ir_learn *learn);

/* Begin a capture. The interrupt handler starts collecting at the next falling
 * edge (the start of a mark), so the stored train always begins with a mark. */
void linkr_ir_learn_begin(struct linkr_ir_learn *learn);

/*
 * Feed one completed interval: `mark` is true when the receiver output was
 * active (carrier present) during `duration_us`. Intervals must alternate
 * mark/space; a mismatch means an edge was lost and the capture is dropped.
 */
enum linkr_ir_learn_step linkr_ir_learn_feed(struct linkr_ir_learn *learn,
					     bool mark, uint32_t duration_us);

/* Silence closed the frame: accept it when enough intervals were captured. */
enum linkr_ir_learn_step linkr_ir_learn_finish(struct linkr_ir_learn *learn);

/* True when the capture holds a replayable frame. */
bool linkr_ir_learn_valid(const struct linkr_ir_learn *learn);

/* Total on-air time of the captured frame, in microseconds. */
uint32_t linkr_ir_learn_total_us(const struct linkr_ir_learn *learn);

/* Serialize to the little-endian storage blob. Returns the blob length, or 0
 * when `capacity` is too small or the capture is not complete. */
size_t linkr_ir_learn_serialize(const struct linkr_ir_learn *learn, uint8_t *blob,
				size_t capacity);

/* Parse a storage blob. Returns 0 on success, -EINVAL when it is not a valid
 * learned command. */
int linkr_ir_learn_parse(struct linkr_ir_learn *learn, const uint8_t *blob,
			 size_t length);

/*
 * Validate the fixed 8-byte header against the total blob length and return
 * the edge count. Booting only needs this much: the waveform stays in NVS
 * until a replay reads the whole blob, so no RAM cache of saved frames is
 * needed. Returns false when the header or the implied length is invalid.
 */
bool linkr_ir_learn_header_edges(const uint8_t *header, size_t total_length,
				 uint16_t *edges);

#endif /* LINKR_IR_LEARN_H_ */
