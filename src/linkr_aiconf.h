/*
 * Board-stored AI configuration blob.
 *
 * The app serialises its model configuration (endpoint, model, protocol,
 * limits and, only when the user opts in, the API key and extra headers) and
 * keeps it in NVS so it follows the board instead of one browser. Firmware
 * treats the bytes as opaque; the app owns the schema and validates the blob
 * after reading it back.
 *
 * Upload is chunked: begin, put, put, ..., end. Reading is chunked as well,
 * because a management response is small. RAM cost is one blob buffer, used for
 * upload staging and for reads.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#ifndef LINKR_AICONF_H_
#define LINKR_AICONF_H_

#include <stddef.h>
#include <stdint.h>

#define LINKR_AICONF_MAX CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG_MAX

/* Stored blob length in bytes, 0 when empty. */
int linkr_aiconf_len(void);

/* Reset the upload staging buffer (no stored data is touched yet). */
int linkr_aiconf_begin(void);

/* Append bytes to the staging buffer. Returns the staged length, -ENOSPC when
 * the blob budget is exceeded and -EINVAL for an empty/invalid request. */
int linkr_aiconf_put(const uint8_t *data, size_t len);

/* Persist the staged blob and end the upload. An empty stage clears it. */
int linkr_aiconf_commit(void);

/* Delete the stored blob and drop any staging state. */
int linkr_aiconf_clear(void);

/* Copy up to `out_len` bytes of the stored blob starting at `offset`.
 * Returns the copied length, -ENOENT when empty, -EBUSY while an upload is in
 * flight, or a negative errno from NVS. */
int linkr_aiconf_read(uint16_t offset, uint8_t *out, size_t out_len);

#endif /* LINKR_AICONF_H_ */
