/*
 * Linkr Bee auxiliary board I/O commands.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_aux.h"

#include <errno.h>
#include <string.h>

#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/printk.h>
#include <zephyr/sys/util.h>

#include "linkr_aiconf.h"
#include "linkr_ir.h"
#include "linkr_opto.h"
#include "linkr_prompt.h"
#include "linkr_status_led.h"

/* Shares main.c's log source: same prefix, no second source slot in RAM. */
LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

#define LINKR_AUX_MAX_REPEATS 16u
/* Kconfig caps slot names at 64 bytes; decode up to that ceiling so an
 * over-long name is reported as too-long instead of unreadable hex. */
#define LINKR_AUX_MAX_NAME_BYTES 64u

/* ---------------------------------------------------------------------- */
/* Small parsers                                                          */
/* ---------------------------------------------------------------------- */

static const char *skip_spaces(const char *text)
{
	while (*text == ' ' || *text == '\t') {
		text++;
	}
	return text;
}

/* Parse one unsigned value, decimal or 0x-prefixed hex, up to `max`. */
static bool parse_uint(const char **cursor, uint32_t max, uint32_t *value)
{
	const char *text = skip_spaces(*cursor);
	uint32_t result = 0;
	uint32_t base = 10;
	bool any = false;

	if (text[0] == '0' && (text[1] == 'x' || text[1] == 'X')) {
		base = 16;
		text += 2;
	}

	while (*text != '\0') {
		uint32_t digit;

		if (*text >= '0' && *text <= '9') {
			digit = (uint32_t)(*text - '0');
		} else if (base == 16 && *text >= 'a' && *text <= 'f') {
			digit = (uint32_t)(*text - 'a' + 10);
		} else if (base == 16 && *text >= 'A' && *text <= 'F') {
			digit = (uint32_t)(*text - 'A' + 10);
		} else {
			break;
		}

		if (result > (max / base)) {
			return false;
		}
		result = result * base + digit;
		any = true;
		text++;
	}

	if (!any || result > max) {
		return false;
	}

	*cursor = text;
	*value = result;
	return true;
}

static bool expect_char(const char **cursor, char expected)
{
	const char *text = skip_spaces(*cursor);

	if (*text != expected) {
		return false;
	}
	*cursor = text + 1;
	return true;
}

/* True when the rest of the line is blank. */
static bool at_end(const char *cursor)
{
	return *skip_spaces(cursor) == '\0';
}

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN) || IS_ENABLED(CONFIG_SETTINGS)
static int hex_value(char digit)
{
	if (digit >= '0' && digit <= '9') {
		return digit - '0';
	}
	if (digit >= 'a' && digit <= 'f') {
		return digit - 'a' + 10;
	}
	if (digit >= 'A' && digit <= 'F') {
		return digit - 'A' + 10;
	}
	return -1;
}

/* Encode bytes as lowercase hex, NUL-terminated. Returns the character count
 * written, or 0 when `out` is too small. */
static size_t hex_encode(const uint8_t *data, size_t length, char *out,
			 size_t out_size)
{
	static const char digits[] = "0123456789abcdef";

	if (length * 2u + 1u > out_size) {
		return 0;
	}
	for (size_t i = 0; i < length; i++) {
		out[i * 2u] = digits[data[i] >> 4];
		out[i * 2u + 1u] = digits[data[i] & 0x0fu];
	}
	out[length * 2u] = '\0';
	return length * 2u;
}

/* Decode an even-length hex string. Returns the byte count, or -1 when the
 * text is not valid hex or does not fit `out`. */
static int hex_decode(const char *text, uint8_t *out, size_t out_size)
{
	size_t length = strlen(text);
	size_t bytes = length / 2u;

	if (length == 0u || (length % 2u) != 0u || bytes > out_size) {
		return -1;
	}
	for (size_t i = 0; i < bytes; i++) {
		int high = hex_value(text[i * 2u]);
		int low = hex_value(text[i * 2u + 1u]);

		if (high < 0 || low < 0) {
			return -1;
		}
		out[i] = (uint8_t)((high << 4) | low);
	}
	return (int)bytes;
}
#endif /* CONFIG_LINKR_BLE_BRIDGE_IR_LEARN */

/* ---------------------------------------------------------------------- */
/* Initialization and help                                                 */
/* ---------------------------------------------------------------------- */

int linkr_aux_init(void)
{
	int first_error = 0;
	int err;

	err = linkr_status_led_init();
	if (err && err != -ENODEV && first_error == 0) {
		first_error = err;
		LOG_WRN("Status LED init failed: %d", err);
	}

	err = linkr_opto_init();
	if (err && err != -ENODEV && first_error == 0) {
		first_error = err;
		LOG_WRN("Optocoupler init failed: %d", err);
	}

	err = linkr_ir_init();
	if (err && err != -ENODEV && first_error == 0) {
		first_error = err;
		LOG_WRN("IR transceiver init failed: %d", err);
	}

	return first_error;
}

void linkr_aux_help(char *buffer, size_t size)
{
	size_t used = 0;

	if (buffer == NULL || size == 0u) {
		return;
	}
	buffer[0] = '\0';

	if (linkr_status_led_available()) {
		used += (size_t)snprintk(buffer + used, size - used,
					 "@l?|@l=<r>,<g>,<b>|@l off ");
	}
	if (linkr_opto_available()) {
		used += (size_t)snprintk(buffer + used, size - used,
					 "@o?|@o on|@o off|@o pulse <ms> ");
	}
	if (linkr_ir_available()) {
		used += (size_t)snprintk(buffer + used, size - used,
					 "@r?|@r tx <addr>,<cmd>[,<repeats>]|"
					 "@r rx on|off|@r clear ");
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
		used += (size_t)snprintk(buffer + used, size - used,
					 "@r learn [slot]|@r learn stop|"
					 "@r play [slot]|@r slots|"
					 "@r delete <slot>|"
					 "@r name <slot> [hex|clear] ");
#endif
	}
#if IS_ENABLED(CONFIG_SETTINGS)
	used += (size_t)snprintk(buffer + used, size - used,
				 "@p?|@p get [off]|@p set <hex>|@p clear ");
#endif
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG)
	used += (size_t)snprintk(buffer + used, size - used,
				 "@y?|@y begin|@y put <hex>|@y end|"
				 "@y get [off]|@y clear ");
#endif
}

/* ---------------------------------------------------------------------- */
/* Status LED                                                              */
/* ---------------------------------------------------------------------- */

static bool led_command(const char *body, char *response, size_t size)
{
	const char *rest;

	if (!strcmp(body, "l?") || !strcmp(body, "led?")) {
		uint8_t r, g, b;

		linkr_status_led_get_rgb(&r, &g, &b);
		snprintk(response, size, "OK led rgb=%02x%02x%02x\r\n", r, g, b);
		return true;
	}

	if (strncmp(body, "l=", 2) != 0 && strncmp(body, "led=", 4) != 0) {
		if (!strcmp(body, "l off") || !strcmp(body, "led off")) {
			if (!linkr_status_led_available()) {
				snprintk(response, size, "ERR led unsupported\r\n");
				return true;
			}
			linkr_status_led_off();
			snprintk(response, size, "OK led off\r\n");
			return true;
		}
		return false;
	}

	rest = (body[0] == 'l' && body[1] == '=') ? body + 2 : body + 4;
	if (!linkr_status_led_available()) {
		snprintk(response, size, "ERR led unsupported\r\n");
		return true;
	}

	uint32_t r, g, b;

	if (!parse_uint(&rest, 255, &r) || !expect_char(&rest, ',') ||
	    !parse_uint(&rest, 255, &g) || !expect_char(&rest, ',') ||
	    !parse_uint(&rest, 255, &b) || !at_end(rest)) {
		snprintk(response, size, "ERR led rgb=expected r,g,b 0-255\r\n");
		return true;
	}

	if (linkr_status_led_set_rgb((uint8_t)r, (uint8_t)g, (uint8_t)b) != 0) {
		snprintk(response, size, "ERR led unsupported\r\n");
		return true;
	}

	snprintk(response, size, "OK led rgb=%02x%02x%02x\r\n", (uint8_t)r,
		 (uint8_t)g, (uint8_t)b);
	return true;
}

/* ---------------------------------------------------------------------- */
/* Optocoupler switch                                                      */
/* ---------------------------------------------------------------------- */

static bool opto_command(const char *body, char *response, size_t size)
{
	const char *rest;

	if (!strcmp(body, "o?") || !strcmp(body, "opto?")) {
		snprintk(response, size, "OK opto=%s\r\n",
			 linkr_opto_is_closed() ? "closed" : "open");
		return true;
	}

	if (!strncmp(body, "o ", 2)) {
		rest = body + 2;
	} else if (!strncmp(body, "opto ", 5)) {
		rest = body + 5;
	} else {
		return false;
	}

	if (!linkr_opto_available()) {
		snprintk(response, size, "ERR opto unsupported\r\n");
		return true;
	}

	rest = skip_spaces(rest);

	if (!strcmp(rest, "on") || !strcmp(rest, "close") ||
	    !strcmp(rest, "closed")) {
		if (linkr_opto_set(true) != 0) {
			snprintk(response, size, "ERR opto failed\r\n");
		} else {
			snprintk(response, size, "OK opto=closed\r\n");
		}
		return true;
	}

	if (!strcmp(rest, "off") || !strcmp(rest, "open") ||
	    !strcmp(rest, "opened")) {
		if (linkr_opto_set(false) != 0) {
			snprintk(response, size, "ERR opto failed\r\n");
		} else {
			snprintk(response, size, "OK opto=open\r\n");
		}
		return true;
	}

	if (!strncmp(rest, "pulse", 5)) {
		uint32_t duration_ms;

		rest += 5;
		if (!parse_uint(&rest, 60000u, &duration_ms) || duration_ms == 0 ||
		    !at_end(rest)) {
			snprintk(response, size,
				 "ERR opto pulse=expected 1-60000 ms\r\n");
			return true;
		}
		if (linkr_opto_pulse(duration_ms) != 0) {
			snprintk(response, size, "ERR opto pulse failed\r\n");
		} else {
			snprintk(response, size, "OK opto=pulse ms=%u\r\n",
				 duration_ms);
		}
		return true;
	}

	snprintk(response, size, "ERR opto=expected on|off|pulse <ms>\r\n");
	return true;
}

/* ---------------------------------------------------------------------- */
/* Board-stored AI prompt                                                  */
/* ---------------------------------------------------------------------- */

#if IS_ENABLED(CONFIG_SETTINGS)
#define LINKR_AUX_PROMPT_CHUNK 48u

static bool prompt_command(const char *body, char *response, size_t size)
{
	const char *rest;

	if (!strcmp(body, "p?") || !strcmp(body, "prompt?")) {
		snprintk(response, size, "OK prompt=%d\r\n", linkr_prompt_len());
		return true;
	}

	if (!strncmp(body, "p ", 2)) {
		rest = skip_spaces(body + 2);
	} else if (!strncmp(body, "prompt ", 7)) {
		rest = skip_spaces(body + 7);
	} else {
		return false;
	}

	if (!strncmp(rest, "get", 3)) {
		const char *value = skip_spaces(rest + 3);
		uint32_t offset = 0;
		uint8_t text[LINKR_PROMPT_MAX];
		char hex[(LINKR_AUX_PROMPT_CHUNK * 2u) + 1u];
		int length;
		size_t chunk;

		if (!at_end(value) &&
		    (!parse_uint(&value, LINKR_PROMPT_MAX, &offset) ||
		     !at_end(value))) {
			snprintk(response, size, "ERR prompt=bad offset\r\n");
			return true;
		}
		length = linkr_prompt_read(text, sizeof(text));
		if (length == -ENOENT) {
			snprintk(response, size, "ERR prompt=empty\r\n");
			return true;
		}
		if (length < 0) {
			snprintk(response, size, "ERR prompt=read-failed\r\n");
			return true;
		}
		if (offset > (uint32_t)length) {
			offset = (uint32_t)length;
		}
		chunk = (size_t)length - offset;
		if (chunk > LINKR_AUX_PROMPT_CHUNK) {
			chunk = LINKR_AUX_PROMPT_CHUNK;
		}
		if (hex_encode(&text[offset], chunk, hex, sizeof(hex)) == 0u &&
		    chunk != 0u) {
			snprintk(response, size, "ERR prompt=hex\r\n");
			return true;
		}
		snprintk(response, size,
			 "OK p off=%u hex=%s total=%d done=%s\r\n", offset, hex,
			 length, offset + chunk >= (size_t)length ? "yes" : "no");
		return true;
	}

	if (!strncmp(rest, "set", 3)) {
		const char *value = skip_spaces(rest + 3);
		uint8_t decoded[LINKR_PROMPT_MAX];
		int length = hex_decode(value, decoded, sizeof(decoded));

		if (length <= 0) {
			snprintk(response, size, "ERR prompt=hex\r\n");
			return true;
		}
		for (int i = 0; i < length; i++) {
			if (decoded[i] == '\0') {
				snprintk(response, size, "ERR prompt=hex\r\n");
				return true;
			}
		}
		if (linkr_prompt_write(decoded, (size_t)length) != 0) {
			snprintk(response, size, "ERR prompt=save-failed\r\n");
			return true;
		}
		snprintk(response, size, "OK prompt len=%d\r\n", length);
		return true;
	}

	if (!strncmp(rest, "clear", 5)) {
		if (!at_end(rest + 5)) {
			snprintk(response, size, "ERR prompt=expected clear\r\n");
			return true;
		}
		(void)linkr_prompt_clear();
		snprintk(response, size, "OK prompt cleared\r\n");
		return true;
	}

	snprintk(response, size, "ERR prompt=expected set|get|clear\r\n");
	return true;
}
#endif /* CONFIG_SETTINGS */

/* ---------------------------------------------------------------------- */
/* Board-stored AI configuration                                           */
/* ---------------------------------------------------------------------- */

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG)
#define LINKR_AUX_AICONF_CHUNK 48u
#define LINKR_AUX_AICONF_PUT_MAX 200u

static bool aiconf_command(const char *body, char *response, size_t size)
{
	const char *rest;

	if (!strcmp(body, "y?") || !strcmp(body, "aiconf?")) {
		snprintk(response, size, "OK aiconf bytes=%d max=%d\r\n",
			 linkr_aiconf_len(), LINKR_AICONF_MAX);
		return true;
	}

	if (!strncmp(body, "y ", 2)) {
		rest = skip_spaces(body + 2);
	} else if (!strncmp(body, "aiconf ", 7)) {
		rest = skip_spaces(body + 7);
	} else {
		return false;
	}

	if (!strcmp(rest, "begin")) {
		(void)linkr_aiconf_begin();
		snprintk(response, size, "OK aiconf begin\r\n");
		return true;
	}

	if (!strncmp(rest, "put", 3)) {
		const char *value = skip_spaces(rest + 3);
		uint8_t decoded[LINKR_AUX_AICONF_PUT_MAX];
		int length = hex_decode(value, decoded, sizeof(decoded));

		if (length <= 0) {
			snprintk(response, size, "ERR aiconf=hex\r\n");
			return true;
		}
		for (int i = 0; i < length; i++) {
			if (decoded[i] == '\0') {
				snprintk(response, size, "ERR aiconf=hex\r\n");
				return true;
			}
		}
		int staged = linkr_aiconf_put(decoded, (size_t)length);

		if (staged == -ENOSPC) {
			snprintk(response, size, "ERR aiconf=too-large\r\n");
		} else if (staged < 0) {
			snprintk(response, size, "ERR aiconf=not-begun\r\n");
		} else {
			snprintk(response, size, "OK aiconf stage=%d\r\n",
				 staged);
		}
		return true;
	}

	if (!strcmp(rest, "end")) {
		int err = linkr_aiconf_commit();

		if (err != 0) {
			snprintk(response, size, "ERR aiconf=save-failed\r\n");
		} else {
			snprintk(response, size, "OK aiconf bytes=%d\r\n",
				 linkr_aiconf_len());
		}
		return true;
	}

	if (!strcmp(rest, "clear")) {
		(void)linkr_aiconf_clear();
		snprintk(response, size, "OK aiconf cleared\r\n");
		return true;
	}

	if (!strncmp(rest, "get", 3)) {
		const char *value = skip_spaces(rest + 3);
		uint32_t offset = 0;
		uint8_t blob[LINKR_AUX_AICONF_CHUNK];
		char hex[(LINKR_AUX_AICONF_CHUNK * 2u) + 1u];
		int length;
		int total = linkr_aiconf_len();

		if (!at_end(value) &&
		    (!parse_uint(&value, LINKR_AICONF_MAX, &offset) ||
		     !at_end(value))) {
			snprintk(response, size, "ERR aiconf=bad offset\r\n");
			return true;
		}
		if (total <= 0) {
			snprintk(response, size, "ERR aiconf=empty\r\n");
			return true;
		}
		length = linkr_aiconf_read((uint16_t)offset, blob,
					   sizeof(blob));
		if (length == -EBUSY) {
			snprintk(response, size, "ERR aiconf=staging\r\n");
			return true;
		}
		if (length < 0) {
			snprintk(response, size,
				 "ERR aiconf=read-failed\r\n");
			return true;
		}
		if (length > 0 &&
		    hex_encode(blob, (size_t)length, hex, sizeof(hex)) == 0u) {
			snprintk(response, size, "ERR aiconf=hex\r\n");
			return true;
		}
		if (length == 0) {
			hex[0] = '\0';
		}
		snprintk(response, size,
			 "OK aiconf off=%u hex=%s total=%d done=%s\r\n",
			 offset, hex, total,
			 offset + (uint32_t)length >= (uint32_t)total ? "yes"
								      : "no");
		return true;
	}

	snprintk(response, size, "ERR aiconf=expected begin|put|end|get|clear\r\n");
	return true;
}
#endif /* CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG */

/* ---------------------------------------------------------------------- */
/* IR transceiver                                                          */
/* ---------------------------------------------------------------------- */

static bool ir_command(const char *body, char *response, size_t size)
{
	const char *rest;

	if (!strcmp(body, "r?") || !strcmp(body, "ir?")) {
		struct linkr_ir_status status;
		char last[56];
		char learn[48];

		linkr_ir_get_status(&status);
		if (status.have_frame && !status.last.repeat) {
			snprintk(last, sizeof(last), "addr=0x%04x,cmd=0x%02x,ext=%s",
				 status.last.address, status.last.command,
				 status.last.extended ? "yes" : "no");
		} else {
			snprintk(last, sizeof(last), "none");
		}
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
		if (status.learning) {
			snprintk(learn, sizeof(learn), "active,slot=%u,edges=%u",
				 status.learn_slot, status.learn_edges);
		} else {
			snprintk(learn, sizeof(learn), "idle,slots=%u/%u",
				 status.slots_used, LINKR_IR_LEARN_SLOTS);
		}
#else
		snprintk(learn, sizeof(learn), "off");
#endif
		snprintk(response, size,
			 "OK ir rx=%s frames=%u repeats=%u glitches=%u last=%s "
			 "learn=%s\r\n",
			 status.rx_enabled ? "on" : "off", status.frames,
			 status.repeats, status.glitches, last, learn);
		return true;
	}

	if (!strncmp(body, "r ", 2)) {
		rest = body + 2;
	} else if (!strncmp(body, "ir ", 3)) {
		rest = body + 3;
	} else {
		return false;
	}

	rest = skip_spaces(rest);

	if (!strncmp(rest, "rx", 2)) {
		const char *value = skip_spaces(rest + 2);
		bool enable;

		if (!strcmp(value, "on")) {
			enable = true;
		} else if (!strcmp(value, "off")) {
			enable = false;
		} else {
			snprintk(response, size, "ERR ir rx=expected on|off\r\n");
			return true;
		}

		if (linkr_ir_set_rx(enable) != 0) {
			snprintk(response, size, "ERR ir unsupported\r\n");
		} else {
			snprintk(response, size, "OK ir rx=%s\r\n",
				 enable ? "on" : "off");
		}
		return true;
	}

	if (!strcmp(rest, "clear")) {
		linkr_ir_clear();
		snprintk(response, size, "OK ir cleared\r\n");
		return true;
	}

	if (!strncmp(rest, "tx", 2) || !strncmp(rest, "send", 4)) {
		uint32_t address, command;
		uint32_t repeats = 0;
		const char *values = rest + (rest[1] == 'x' ? 2 : 4);

		if (!parse_uint(&values, 0xffffu, &address) ||
		    !expect_char(&values, ',') ||
		    !parse_uint(&values, 0xffu, &command)) {
			snprintk(response, size,
				 "ERR ir tx=expected <addr>,<cmd>[,<repeats>]\r\n");
			return true;
		}

		if (expect_char(&values, ',')) {
			if (!parse_uint(&values, LINKR_AUX_MAX_REPEATS, &repeats)) {
				snprintk(response, size,
					 "ERR ir repeats=0-%u\r\n",
					 LINKR_AUX_MAX_REPEATS);
				return true;
			}
		}

		if (!at_end(values)) {
			snprintk(response, size,
				 "ERR ir tx=expected <addr>,<cmd>[,<repeats>]\r\n");
			return true;
		}

		int err = linkr_ir_send((uint16_t)address, (uint8_t)command,
					(uint8_t)repeats);

		if (err == -ENODEV) {
			snprintk(response, size, "ERR ir unsupported\r\n");
		} else if (err != 0) {
			snprintk(response, size, "ERR ir busy\r\n");
		} else {
			snprintk(response, size,
				 "OK ir tx addr=0x%04x,cmd=0x%02x,repeats=%u\r\n",
				 address, command, repeats);
		}
		return true;
	}

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
	if (!strncmp(rest, "learn", 5)) {
		const char *value = skip_spaces(rest + 5);
		struct linkr_ir_status status;

		if (!strcmp(value, "stop")) {
			int err = linkr_ir_learn_stop();

			linkr_ir_get_status(&status);
			if (err == 0) {
				snprintk(response, size,
					 "OK ir learn slot=%u stored edges=%u\r\n",
					 status.learn_slot,
					 linkr_ir_slot_edges(status.learn_slot));
			} else if (err == -EAGAIN) {
				snprintk(response, size,
					 "ERR ir learn=no-frame\r\n");
			} else if (err == -ENODEV) {
				snprintk(response, size, "ERR ir unsupported\r\n");
			} else {
				snprintk(response, size, "ERR ir learn=idle\r\n");
			}
			return true;
		}

		if (at_end(value)) {
			uint32_t slot = 0;

			if (linkr_ir_learn_start((uint8_t)slot) != 0) {
				snprintk(response, size, "ERR ir unsupported\r\n");
				return true;
			}
			snprintk(response, size,
				 "OK ir learn slot=%u timeout_ms=%u\r\n", slot,
				 CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_TIMEOUT_MS);
			return true;
		}

		uint32_t slot;

		if (!parse_uint(&value, LINKR_IR_LEARN_SLOTS - 1u, &slot) ||
		    !at_end(value)) {
			snprintk(response, size, "ERR ir slot=0-%u\r\n",
				 LINKR_IR_LEARN_SLOTS - 1u);
			return true;
		}
		if (linkr_ir_learn_start((uint8_t)slot) != 0) {
			snprintk(response, size, "ERR ir unsupported\r\n");
			return true;
		}
		snprintk(response, size, "OK ir learn slot=%u timeout_ms=%u\r\n",
			 slot, CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_TIMEOUT_MS);
		return true;
	}

	if (!strncmp(rest, "play", 4)) {
		const char *value = skip_spaces(rest + 4);
		uint32_t slot = 0;
		uint16_t edges;
		int err;

		if (!at_end(value) &&
		    (!parse_uint(&value, LINKR_IR_LEARN_SLOTS - 1u, &slot) ||
		     !at_end(value))) {
			snprintk(response, size, "ERR ir slot=0-%u\r\n",
				 LINKR_IR_LEARN_SLOTS - 1u);
			return true;
		}

		edges = linkr_ir_slot_edges((uint8_t)slot);
		err = linkr_ir_play((uint8_t)slot);
		if (err == -ENOENT) {
			snprintk(response, size, "ERR ir slot=%u empty\r\n",
				 slot);
		} else if (err == -ENODEV) {
			snprintk(response, size, "ERR ir unsupported\r\n");
		} else if (err != 0) {
			snprintk(response, size, "ERR ir busy\r\n");
		} else {
			snprintk(response, size, "OK ir play slot=%u edges=%u\r\n",
				 slot, edges);
		}
		return true;
	}

	if (!strcmp(rest, "slots")) {
		size_t used = 0;
		struct linkr_ir_status status;

		linkr_ir_get_status(&status);
		used = (size_t)snprintk(response, size, "OK ir slots=");
		for (uint8_t slot = 0;
		     slot < LINKR_IR_LEARN_SLOTS &&
		     used + 16u < size; slot++) {
			used += (size_t)snprintk(
				response + used, size - used, "%s%u:%u",
				slot == 0 ? "" : ",", slot,
				status.slot_edges[slot]);
		}
		snprintk(response + used, size - used, " used=%u\r\n",
			 status.slots_used);
		return true;
	}

	if (!strncmp(rest, "delete", 6) || !strncmp(rest, "forget", 6)) {
		const char *value = skip_spaces(rest + 6);
		uint32_t slot;
		int err;

		if (!parse_uint(&value, LINKR_IR_LEARN_SLOTS - 1u, &slot) ||
		    !at_end(value)) {
			snprintk(response, size, "ERR ir slot=0-%u\r\n",
				 LINKR_IR_LEARN_SLOTS - 1u);
			return true;
		}
		err = linkr_ir_forget((uint8_t)slot);
		if (err == -ENOENT) {
			snprintk(response, size, "ERR ir slot=%u empty\r\n",
				 slot);
		} else if (err != 0) {
			snprintk(response, size, "ERR ir unsupported\r\n");
		} else {
			snprintk(response, size, "OK ir slot=%u cleared\r\n",
				 slot);
		}
		return true;
	}

	if (!strncmp(rest, "name", 4)) {
		const char *value = skip_spaces(rest + 4);
		uint32_t slot;
		int length;

		if (!parse_uint(&value, LINKR_IR_LEARN_SLOTS - 1u, &slot)) {
			snprintk(response, size, "ERR ir slot=0-%u\r\n",
				 LINKR_IR_LEARN_SLOTS - 1u);
			return true;
		}
		value = skip_spaces(value);

		if (at_end(value)) {
			char saved[CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX + 1];
			char hex[(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX * 2) + 1];

			length = linkr_ir_slot_name_get((uint8_t)slot, saved,
							sizeof(saved));
			if (length == -ENODEV) {
				snprintk(response, size,
					 "ERR ir unsupported\r\n");
			} else if (length < 0) {
				snprintk(response, size,
					 "ERR ir name=read-failed\r\n");
			} else if (length == 0) {
				snprintk(response, size,
					 "OK ir name=%u none\r\n", slot);
			} else if (hex_encode((const uint8_t *)saved,
					      (size_t)length, hex,
					      sizeof(hex)) == 0) {
				snprintk(response, size,
					 "ERR ir name=encode\r\n");
			} else {
				snprintk(response, size,
					 "OK ir name=%u hex=%s\r\n", slot,
					 hex);
			}
			return true;
		}

		if (!strcmp(value, "clear")) {
			int err = linkr_ir_slot_name_set((uint8_t)slot, NULL);

			if (err == 0) {
				snprintk(response, size,
					 "OK ir name=%u cleared\r\n", slot);
			} else if (err == -ENODEV) {
				snprintk(response, size,
					 "ERR ir unsupported\r\n");
			} else {
				snprintk(response, size,
					 "ERR ir name=save-failed\r\n");
			}
			return true;
		}

		uint8_t decoded[LINKR_AUX_MAX_NAME_BYTES];
		char name[LINKR_AUX_MAX_NAME_BYTES + 1];

		length = hex_decode(value, decoded, sizeof(decoded));
		if (length < 0) {
			snprintk(response, size, "ERR ir name=hex\r\n");
			return true;
		}
		for (int i = 0; i < length; i++) {
			if (decoded[i] == '\0') {
				snprintk(response, size,
					 "ERR ir name=hex\r\n");
				return true;
			}
		}
		memcpy(name, decoded, (size_t)length);
		name[length] = '\0';

		int err = linkr_ir_slot_name_set((uint8_t)slot, name);

		if (err == 0) {
			snprintk(response, size, "OK ir name=%u set\r\n",
				 slot);
		} else if (err == -EINVAL) {
			snprintk(response, size, "ERR ir name=too-long\r\n");
		} else if (err == -ENODEV) {
			snprintk(response, size, "ERR ir unsupported\r\n");
		} else {
			snprintk(response, size,
				 "ERR ir name=save-failed\r\n");
		}
		return true;
	}
#endif /* CONFIG_LINKR_BLE_BRIDGE_IR_LEARN */

	snprintk(response, size,
		 "ERR ir=expected tx <addr>,<cmd>[,<repeats>]|rx on|off|clear"
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
		 "|learn [slot]|learn stop|play [slot]|slots|delete <slot>|"
		 "name <slot> [hex|clear]"
#endif
		 "\r\n");
	return true;
}

bool linkr_aux_is_readonly(const char *body)
{
	const char *rest = NULL;
	uint32_t offset;

	if (body == NULL) {
		return false;
	}
	if (!strcmp(body, "l?") || !strcmp(body, "led?") ||
	    !strcmp(body, "o?") || !strcmp(body, "opto?") ||
	    !strcmp(body, "r?") || !strcmp(body, "ir?") ||
	    !strcmp(body, "r slots") || !strcmp(body, "ir slots") ||
	    !strcmp(body, "p?") || !strcmp(body, "prompt?") ||
	    !strcmp(body, "y?") || !strcmp(body, "aiconf?")) {
		return true;
	}
	if (!strncmp(body, "p get", 5)) {
		rest = body + 5;
	} else if (!strncmp(body, "prompt get", 10)) {
		rest = body + 10;
	} else if (!strncmp(body, "y get", 5)) {
		rest = body + 5;
	} else if (!strncmp(body, "aiconf get", 10)) {
		rest = body + 10;
	}
	if (rest == NULL) {
		return false;
	}
	if (*rest == '\0') {
		return true;
	}
	if (*rest != ' ' && *rest != '\t') {
		return false;
	}
	return parse_uint(&rest, UINT16_MAX, &offset) && at_end(rest);
}

bool linkr_aux_command(const char *body, char *response, size_t response_size)
{
	if (body == NULL || response == NULL || response_size == 0) {
		return false;
	}

	if (body[0] == 'l') {
		return led_command(body, response, response_size);
	}
	if (body[0] == 'o') {
		return opto_command(body, response, response_size);
	}
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG)
	if (body[0] == 'y' || !strncmp(body, "aiconf", 6)) {
		if (aiconf_command(body, response, response_size)) {
			return true;
		}
	}
#endif
#if IS_ENABLED(CONFIG_SETTINGS)
	if (body[0] == 'p' || !strncmp(body, "prompt", 6)) {
		if (prompt_command(body, response, response_size)) {
			return true;
		}
	}
#endif
	if (body[0] == 'r' || (body[0] == 'i' && body[1] == 'r')) {
		/* "ir ..." is the long form; "i?"/"info?" stay with main.c. */
		return ir_command(body, response, response_size);
	}

	return false;
}
