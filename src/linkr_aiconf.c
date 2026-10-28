/*
 * Board-stored AI configuration blob backed by Zephyr settings (NVS).
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_aiconf.h"

#include <errno.h>
#include <string.h>

#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/settings/settings.h>

LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

#define AICONF_KEY "linkr_aiconf/blob"

/* One buffer serves upload staging and reads: the command layer never stages
 * and reads at the same time. */
static uint8_t aiconf_buf[LINKR_AICONF_MAX];
static size_t aiconf_stage;
static size_t aiconf_cached_len;
static bool aiconf_staging;
static bool aiconf_cache_valid;

int linkr_aiconf_len(void)
{
	ssize_t length;

	if (aiconf_cache_valid) {
		return (int)aiconf_cached_len;
	}
	length = settings_get_val_len(AICONF_KEY);
	if (length <= 0) {
		aiconf_cached_len = 0;
		aiconf_cache_valid = true;
		return 0;
	}
	aiconf_cached_len = (size_t)length > LINKR_AICONF_MAX
				    ? LINKR_AICONF_MAX
				    : (size_t)length;
	/* Only a successful read or commit populates the byte cache. */
	return (int)aiconf_cached_len;
}

int linkr_aiconf_begin(void)
{
	aiconf_stage = 0;
	aiconf_staging = true;
	aiconf_cache_valid = false;
	return 0;
}

int linkr_aiconf_put(const uint8_t *data, size_t len)
{
	if (!aiconf_staging) {
		return -EINVAL;
	}
	if (data == NULL || len == 0u) {
		return -EINVAL;
	}
	if (aiconf_stage + len > LINKR_AICONF_MAX) {
		return -ENOSPC;
	}
	memcpy(aiconf_buf + aiconf_stage, data, len);
	aiconf_stage += len;
	return (int)aiconf_stage;
}

int linkr_aiconf_commit(void)
{
	int err;

	if (!aiconf_staging) {
		return -EINVAL;
	}
	aiconf_staging = false;
	aiconf_cache_valid = false;
	if (aiconf_stage == 0u) {
		(void)settings_delete(AICONF_KEY);
		aiconf_cached_len = 0;
		aiconf_cache_valid = true;
		return 0;
	}
	err = settings_save_one(AICONF_KEY, aiconf_buf, aiconf_stage);
	if (err) {
		LOG_WRN("AI config save failed: %d", err);
		return -EIO;
	}
	aiconf_cached_len = aiconf_stage;
	aiconf_cache_valid = true;
	LOG_INF("AI config stored (%u bytes)", (unsigned int)aiconf_stage);
	return 0;
}

int linkr_aiconf_clear(void)
{
	aiconf_stage = 0;
	aiconf_staging = false;
	aiconf_cached_len = 0;
	aiconf_cache_valid = true;
	(void)settings_delete(AICONF_KEY);
	return 0;
}

int linkr_aiconf_read(uint16_t offset, uint8_t *out, size_t out_len)
{
	ssize_t length;
	size_t copy;

	if (aiconf_staging) {
		return -EBUSY;
	}
	if (out == NULL || out_len == 0u) {
		return -EINVAL;
	}
	if (!aiconf_cache_valid) {
		length = settings_load_one(AICONF_KEY, aiconf_buf,
					   sizeof(aiconf_buf));
		if (length == -ENOENT) {
			aiconf_cached_len = 0;
			aiconf_cache_valid = true;
			return -ENOENT;
		}
		if (length < 0) {
			return (int)length;
		}
		if ((size_t)length > sizeof(aiconf_buf)) {
			length = (ssize_t)sizeof(aiconf_buf);
		}
		aiconf_cached_len = (size_t)length;
		aiconf_cache_valid = true;
	}
	if (aiconf_cached_len == 0u) {
		return -ENOENT;
	}
	if (offset >= aiconf_cached_len) {
		return 0;
	}
	copy = aiconf_cached_len - offset;
	if (copy > out_len) {
		copy = out_len;
	}
	memcpy(out, aiconf_buf + offset, copy);
	return (int)copy;
}
