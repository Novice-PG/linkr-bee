/*
 * Board-stored AI prompt (one text).
 *
 * The assistant keeps its custom instructions on the Linkr Bee itself instead
 * of only in one browser: the text is stored in NVS and read when a
 * conversation starts, so it travels with the board. RAM cost is zero: the
 * settings backend holds the value and the command layer reads it on demand.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#ifndef LINKR_PROMPT_H_
#define LINKR_PROMPT_H_

#include <stddef.h>
#include <stdint.h>

/* Upper bound in UTF-8 bytes: fits one management command as hex. */
#define LINKR_PROMPT_MAX 240

/* Length of the stored prompt, 0 when empty. */
int linkr_prompt_len(void);

/* Read the prompt into `out` (no NUL appended). Returns its byte length,
 * -ENOENT when empty, -ENOSPC when `out` is too small. */
int linkr_prompt_read(uint8_t *out, size_t out_len);

/* Store `len` bytes (1..LINKR_PROMPT_MAX); empty clears it. */
int linkr_prompt_write(const uint8_t *data, size_t len);

/* Remove the stored prompt. */
int linkr_prompt_clear(void);

#endif /* LINKR_PROMPT_H_ */
