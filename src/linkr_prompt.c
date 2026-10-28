/*
 * Board-stored AI prompt backed by Zephyr settings (NVS).
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_prompt.h"

#include <errno.h>

#include <zephyr/logging/log.h>
#include <zephyr/settings/settings.h>

LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

#define PROMPT_KEY "linkr_prompt/text"

int linkr_prompt_len(void)
{
	ssize_t length = settings_get_val_len(PROMPT_KEY);

	if (length <= 0) {
		return 0;
	}
	return length > LINKR_PROMPT_MAX ? LINKR_PROMPT_MAX : (int)length;
}

int linkr_prompt_read(uint8_t *out, size_t out_len)
{
	ssize_t length;

	if (out == NULL || out_len == 0u) {
		return -ENOSPC;
	}
	length = settings_load_one(PROMPT_KEY, out, out_len);
	if (length == -ENOENT) {
		return -ENOENT;
	}
	if (length < 0) {
		return (int)length;
	}
	if ((size_t)length > out_len) {
		return -ENOSPC;
	}
	return (int)length;
}

int linkr_prompt_write(const uint8_t *data, size_t len)
{
	if (len == 0u) {
		return linkr_prompt_clear();
	}
	if (len > LINKR_PROMPT_MAX || data == NULL) {
		return -EINVAL;
	}
	if (settings_save_one(PROMPT_KEY, data, len) != 0) {
		LOG_WRN("prompt save failed (%u bytes)", (unsigned int)len);
		return -EIO;
	}
	return 0;
}

int linkr_prompt_clear(void)
{
	(void)settings_delete(PROMPT_KEY);
	return 0;
}
