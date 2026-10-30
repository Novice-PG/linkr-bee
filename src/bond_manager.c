/* SPDX-License-Identifier: Apache-2.0 */
#include <errno.h>
#include <string.h>
#include <zephyr/bluetooth/bluetooth.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>
#include "bond_manager.h"
#include "bond_label.h"

/* Serialize the short delayed removal with queries and other deletion requests.
 * The delay lets the requester receive the acknowledgement before bt_unpair()
 * disconnects a currently connected peer. OK queued is not a completion report. */
K_MUTEX_DEFINE(bond_lock);
static bool pending;
static int last_result;
static uint8_t pending_id;
static bt_addr_le_t pending_addr;
static void remove_bond(struct k_work *work)
{
	ARG_UNUSED(work);
	k_mutex_lock(&bond_lock, K_FOREVER);
	last_result = bt_unpair(pending_id, &pending_addr);
	/* A successful unpair (or a bond that is already gone) leaves the label
	 * orphaned, so drop it too. A transient failure keeps the bond, and with
	 * it the label. */
	if (last_result == 0 || last_result == -ENOENT) {
		(void)linkr_bond_label_clear(&pending_addr);
	}
	pending = false;
	k_mutex_unlock(&bond_lock);
}
K_WORK_DELAYABLE_DEFINE(remove_work, remove_bond);

struct listing {
	char *reply;
	size_t size, used;
	const bt_addr_le_t *self;
	bool overflow;
};
static void list_bond(const struct bt_bond_info *bond, void *data)
{
	struct listing *list = data;
	char address[BT_ADDR_STR_LEN];
	bt_addr_to_str(&bond->addr.a, address, sizeof(address));
	int n = snprintk(list->reply + list->used, list->size - list->used,
		"BOND %u/%s self=%u\n", bond->addr.type, address,
		bt_addr_le_eq(&bond->addr, list->self) ? 1U : 0U);
	if (n < 0 || (size_t)n >= list->size - list->used) {
		list->overflow = true;
		return;
	}
	list->used += n;
}

/* Parse "<0|1>/AA:BB:CC:DD:EE:FF" (exactly 19 bytes) into an identity
 * address. The token is copied first so a following command argument cannot
 * bleed into the address parser. */
static bool parse_address(const char *value, bt_addr_le_t *out)
{
	char text[BT_ADDR_STR_LEN];

	if (strlen(value) != 19u) {
		return false;
	}
	if (value[0] != '0' && value[0] != '1') {
		return false;
	}
	if (value[1] != '/') {
		return false;
	}
	memcpy(text, value + 2, 17u);
	text[17] = '\0';
	return bt_addr_le_from_str(text, value[0] == '0' ? "public" : "random",
				   out) == 0;
}

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
 * written, or 0 when out is too small. */
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
 * text is not valid hex or does not fit out. */
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

/* A label is free text that ends up on a terminal, so refuse NUL and the C0
 * control block as well as DEL: ESC sequences are a real injection surface in
 * the TUI and CLI. */
static bool label_bytes_ok(const uint8_t *bytes, size_t length)
{
	for (size_t i = 0; i < length; i++) {
		if (bytes[i] < 0x20u || bytes[i] == 0x7fu) {
			return false;
		}
	}
	return length > 0u;
}

/* "bonds label <type>/<addr> [<hex>|clear]". args points just past the
 * "bonds label " prefix. Labels are asked for one address at a time so the
 * fixed reply buffer never has to hold all of them. */
static void bond_label_command(uint8_t id, const char *args, char *reply,
			       size_t size)
{
	char address_token[BT_ADDR_STR_LEN + 2];
	bt_addr_le_t address;
	char address_text[BT_ADDR_STR_LEN];
	const char *value;
	size_t args_len = strlen(args);

	if (args_len < 19u) {
		snprintk(reply, size, "ERR bonds address\n");
		return;
	}
	memcpy(address_token, args, 19u);
	address_token[19] = '\0';
	if (!parse_address(address_token, &address)) {
		snprintk(reply, size, "ERR bonds address\n");
		return;
	}
	if (args[19] != '\0' && args[19] != ' ') {
		snprintk(reply, size, "ERR bonds address\n");
		return;
	}
	if (!bt_le_bond_exists(id, &address)) {
		snprintk(reply, size, "ERR bonds not found\n");
		return;
	}
	bt_addr_to_str(&address.a, address_text, sizeof(address_text));

	value = args + 19;
	while (*value == ' ') {
		value++;
	}

	if (*value == '\0') {
		char out[CONFIG_LINKR_BLE_BRIDGE_BOND_LABEL_MAX + 1];
		int n = linkr_bond_label_get(&address, out, sizeof(out));

		if (n < 0) {
			snprintk(reply, size, "ERR bonds save=%d\n", n);
			return;
		}
		if (n == 0) {
			snprintk(reply, size, "OK bonds label=%u/%s none\n",
				 (unsigned int)address.type, address_text);
		} else {
			char hex[CONFIG_LINKR_BLE_BRIDGE_BOND_LABEL_MAX * 2 + 1];

			hex_encode((const uint8_t *)out, (size_t)n, hex,
				   sizeof(hex));
			snprintk(reply, size,
				 "OK bonds label=%u/%s hex=%s\n",
				 (unsigned int)address.type, address_text, hex);
		}
		return;
	}

	if (!strcmp(value, "clear")) {
		(void)linkr_bond_label_clear(&address);
		snprintk(reply, size, "OK bonds label=%u/%s none\n",
			 (unsigned int)address.type, address_text);
		return;
	}

	/* Hex-encoded UTF-8 write. */
	{
		size_t value_len = strlen(value);
		uint8_t bytes[CONFIG_LINKR_BLE_BRIDGE_BOND_LABEL_MAX];
		char hex[CONFIG_LINKR_BLE_BRIDGE_BOND_LABEL_MAX * 2 + 1];
		int n;

		if (value_len == 0u || (value_len % 2u) != 0u) {
			snprintk(reply, size, "ERR bonds label hex\n");
			return;
		}
		if (value_len / 2u > CONFIG_LINKR_BLE_BRIDGE_BOND_LABEL_MAX) {
			snprintk(reply, size, "ERR bonds label length\n");
			return;
		}
		n = hex_decode(value, bytes, sizeof(bytes));
		if (n < 0) {
			snprintk(reply, size, "ERR bonds label hex\n");
			return;
		}
		if (!label_bytes_ok(bytes, (size_t)n)) {
			snprintk(reply, size, "ERR bonds label utf8\n");
			return;
		}
		if (linkr_bond_label_set(&address, (const char *)bytes,
					 (size_t)n) < 0) {
			snprintk(reply, size, "ERR bonds save\n");
			return;
		}
		hex_encode(bytes, (size_t)n, hex, sizeof(hex));
		snprintk(reply, size, "OK bonds label=%u/%s hex=%s\n",
			 (unsigned int)address.type, address_text, hex);
	}
}

bool linkr_bond_command(struct bt_conn *conn, const char *command, char *reply, size_t size)
{
	bool query = !strcmp(command, "bonds?");
	bool label = !strncmp(command, "bonds label ", 12);
	bool remove = !strncmp(command, "bonds remove ", 13);

	if (!query && !label && !remove) return false;
	struct bt_conn_info info;
	if (!conn || bt_conn_get_info(conn, &info) || bt_conn_get_security(conn) < BT_SECURITY_L2) {
		snprintk(reply, size, "ERR bonds encrypted BLE required\n");
		return true;
	}
	if (label) {
		/* Straight to NVS, without bond_lock: a flash write must not
		 * stall the delayed removal. */
		bond_label_command(info.id, command + 12, reply, size);
		return true;
	}
	k_mutex_lock(&bond_lock, K_FOREVER);
	if (query) {
		struct listing list = { .reply = reply, .size = size, .self = bt_conn_get_dst(conn) };
		int n = snprintk(reply, size, "OK bonds pending=%u result=%d\n", pending ? 1U : 0U, last_result);
		if (n < 0 || (size_t)n >= size) {
			snprintk(reply, size, "ERR bonds buffer\n");
		} else {
			list.used = n;
			bt_foreach_bond(info.id, list_bond, &list);
			if (list.overflow) snprintk(reply, size, "ERR bonds buffer\n");
		}
	} else {
		const char *value = command + 13;
		bt_addr_le_t address;
		/* Exact identity address, never a shifting list index or wildcard. */
		if (!parse_address(value, &address)) {
			snprintk(reply, size, "ERR bonds address\n");
		} else if (pending) {
			snprintk(reply, size, "ERR bonds busy\n");
		} else if (!bt_le_bond_exists(info.id, &address)) {
			snprintk(reply, size, "ERR bonds not found\n");
		} else {
			pending_id = info.id;
			bt_addr_le_copy(&pending_addr, &address);
			pending = true;
			int err = k_work_reschedule(&remove_work, K_MSEC(500));
			if (err < 0) {
				pending = false;
				snprintk(reply, size, "ERR bonds schedule=%d\n", err);
			} else {
				snprintk(reply, size, "OK bonds queued\n");
			}
		}
	}
	k_mutex_unlock(&bond_lock);
	return true;
}

bool linkr_bond_is_readonly(const char *command)
{
	if (!strcmp(command, "bonds?")) {
		return true;
	}
	if (strncmp(command, "bonds label ", 12) != 0) {
		return false;
	}
	/* Only the bare address (no value) is a read. */
	return strlen(command) == 12u + 19u;
}
