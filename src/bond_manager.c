/* SPDX-License-Identifier: Apache-2.0 */
#include <errno.h>
#include <string.h>
#include <zephyr/bluetooth/bluetooth.h>
#include <zephyr/kernel.h>
#include <zephyr/settings/settings.h>
#include <zephyr/sys/printk.h>
#include "bond_manager.h"

/* ---- per-bond host labels ------------------------------------------------ */

/* A label is the name a host reports for itself, stored next to the bond it
 * owns so the management card can say "longlong-mbp" instead of an address.
 * One blob covers all bonds: a fixed layout beats a settings key per bond, which
 * would need dynamic key names and a key parser. Bump the version when the
 * layout changes; an unknown version or a short read is discarded. */
#define LABEL_SLOTS 8U			/* keep equal to CONFIG_BT_MAX_PAIRED */
#define LABEL_MAX 32U			/* UTF-8 bytes, never cut mid-character */
#define LABEL_KEY "linkr_bond/labels"
#define LABEL_VERSION 2U
#define LABEL_ADDR BT_ADDR_STR_LEN	/* "AA:BB:CC:DD:EE:FF" plus NUL */
#define LABEL_SLOT (1U + LABEL_ADDR + 1U + LABEL_MAX + 1U)
#define LABEL_BLOB (2U + (LABEL_SLOTS * LABEL_SLOT))

static uint8_t label_blob[LABEL_BLOB];
static bool label_ready;

static uint8_t *label_slot(size_t index)
{
	return label_blob + 2U + (index * LABEL_SLOT);
}

static void labels_reset(void)
{
	memset(label_blob, 0, sizeof(label_blob));
	label_blob[0] = LABEL_VERSION;
	label_blob[1] = 0U;
}

static void labels_load(void)
{
	ssize_t length = settings_load_one(LABEL_KEY, label_blob, sizeof(label_blob));

	if (length < 0 || (size_t)length != sizeof(label_blob) ||
	    label_blob[0] != LABEL_VERSION || label_blob[1] > LABEL_SLOTS) {
		labels_reset();
		return;
	}
	/* Settings may contain a torn or incompatible record. Validate all string
	 * bounds before label_find()/name_line() use strcmp or %s. */
	for (size_t i = 0; i < label_blob[1]; i++) {
		const uint8_t *slot = label_slot(i);
		const uint8_t *address = slot + 1U;
		const char *name = (const char *)(slot + 2U + LABEL_ADDR);
		size_t len = slot[1U + LABEL_ADDR];
		bool valid = slot[0] <= 1U && address[LABEL_ADDR - 1U] == 0U &&
			len > 0U && len <= LABEL_MAX;

		for (size_t j = 0; j < LABEL_ADDR - 1U; j++) {
			uint8_t c = address[j];
			valid = valid && (j % 3U == 2U ? c == ':' :
				((c >= '0' && c <= '9') || (c >= 'A' && c <= 'F')));
		}
		if (valid) {
			valid = name[len] == '\0' && memchr(name, '\0', len) == NULL;
			for (size_t j = 0; j < len; j++) {
				unsigned char c = (unsigned char)name[j];
				valid = valid && c >= 0x20U && c != 0x7fU && c != '"' && c != '\\';
			}
		}
		if (!valid) {
			labels_reset();
			return;
		}
	}
}

static void labels_ensure(void)
{
	if (!label_ready) {
		labels_load();
		label_ready = true;
	}
}

static int labels_save(void)
{
	int err = settings_save_one(LABEL_KEY, label_blob, sizeof(label_blob));

	/* A failed write leaves the RAM copy ahead of the flash copy. Drop the cache
	 * so the next reader reloads what is actually stored. */
	if (err != 0) {
		label_ready = false;
	}
	return err;
}

/* Trim a host-provided name into something the listing can carry. Control
 * characters, quotes and backslashes would break the line format; runs of
 * spaces are collapsed, and a UTF-8 sequence cut by the length cap is dropped
 * whole rather than left half-written. */
static size_t label_clean(const char *in, char *out, size_t cap)
{
	size_t len = 0U;

	if (in == NULL || out == NULL || cap < 2U) {
		return 0U;
	}
	for (; *in != '\0' && len + 1U < cap; in++) {
		unsigned char c = (unsigned char)*in;

		if (c == '\t') {
			c = ' ';
		}
		if (c < 0x20U || c == 0x7fU || c == '"' || c == '\\') {
			continue;
		}
		if (c == ' ' && (len == 0U || out[len - 1U] == ' ')) {
			continue;
		}
		out[len++] = (char)c;
	}
	while (len > 0U && out[len - 1U] == ' ') {
		len--;
	}
	if (*in != '\0' && len > 0U) {
		/* The cap stopped us mid-character; is the last one complete? */
		size_t start = len;

		while (start > 0U && ((unsigned char)out[start - 1U] & 0xc0U) == 0x80U) {
			start--;
		}
		if (start > 0U) {
			unsigned char lead = (unsigned char)out[start - 1U];
			size_t need = (lead >= 0xf0U) ? 4U : (lead >= 0xe0U) ? 3U :
				      (lead >= 0xc0U) ? 2U : 1U;

			if (len - (start - 1U) < need) {
				len = start - 1U;
			}
		}
	}
	out[len] = '\0';
	return len;
}

static int label_find(uint8_t type, const char *address)
{
	for (size_t i = 0; i < label_blob[1]; i++) {
		const uint8_t *slot = label_slot(i);

		if (slot[0] == type && !strcmp((const char *)(slot + 1U), address)) {
			return (int)i;
		}
	}
	return -1;
}

static void label_drop(size_t index)
{
	uint8_t count = label_blob[1];

	if (index >= count) {
		return;
	}
	for (size_t i = index; i + 1U < count; i++) {
		memcpy(label_slot(i), label_slot(i + 1U), LABEL_SLOT);
	}
	memset(label_slot(count - 1U), 0, LABEL_SLOT);
	label_blob[1] = (uint8_t)(count - 1U);
}

static int label_set(uint8_t type, const char *address, const char *name)
{
	char clean[LABEL_MAX + 1U];
	size_t len = label_clean(name, clean, sizeof(clean));
	int index = label_find(type, address);
	uint8_t *slot;

	if (len == 0U) {
		return -EINVAL;
	}
	if (index < 0) {
		uint8_t count = label_blob[1];

		if (count >= LABEL_SLOTS) {
			return -ENOSPC;
		}
		index = (int)count;
		label_blob[1] = (uint8_t)(count + 1U);
	}
	slot = label_slot((size_t)index);
	/* Reconnects report the same label; do not wear flash for an unchanged name. */
	if (slot[1U + LABEL_ADDR] == len &&
	    !memcmp(slot + 2U + LABEL_ADDR, clean, len + 1U)) {
		return 0;
	}
	slot[0] = type;
	snprintk((char *)(slot + 1U), LABEL_ADDR, "%s", address);
	slot[1U + LABEL_ADDR] = (uint8_t)len;
	memcpy(slot + 2U + LABEL_ADDR, clean, len + 1U);
	return labels_save();
}

struct live_bonds {
	uint8_t count;
	struct {
		uint8_t type;
		char address[LABEL_ADDR];
	} items[LABEL_SLOTS];
};

static void collect_bond(const struct bt_bond_info *bond, void *data)
{
	struct live_bonds *live = data;
	char address[BT_ADDR_STR_LEN];

	if (live->count >= LABEL_SLOTS) {
		return;
	}
	bt_addr_to_str(&bond->addr.a, address, sizeof(address));
	live->items[live->count].type = bond->addr.type;
	snprintk(live->items[live->count].address, LABEL_ADDR, "%s", address);
	live->count++;
}

static bool live_has(const struct live_bonds *live, uint8_t type, const char *address)
{
	for (uint8_t i = 0; i < live->count; i++) {
		if (live->items[i].type == type && !strcmp(live->items[i].address, address)) {
			return true;
		}
	}
	return false;
}

/* A bond can go away without going through bonds remove: a host that forgets the
 * pairing, or a build with CONFIG_BT_KEYS_OVERWRITE_OLDEST. Drop those labels the
 * first time somebody asks for the list instead of keeping stale text forever. */
static void labels_prune(uint8_t id)
{
	struct live_bonds live = { 0 };
	bool changed = false;

	bt_foreach_bond(id, collect_bond, &live);
	for (size_t i = 0; i < label_blob[1];) {
		const uint8_t *slot = label_slot(i);

		if (live_has(&live, slot[0], (const char *)(slot + 1U))) {
			i++;
		} else {
			label_drop(i);
			changed = true;
		}
	}
	if (changed) {
		(void)labels_save();
	}
}

/* One names line. The <type>/<address> prefix matches the BOND lines so a client
 * can join the two by id; the label is quoted because it may contain spaces. */
static int name_line(char *buf, size_t cap, uint8_t type, const char *address,
		     const char *label)
{
	return snprintk(buf, cap, "NAME %u/%s \"%s\"\n", type, address, label);
}

/* ---- bond list and removal ----------------------------------------------- */

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
	if (last_result == 0) {
		char address[BT_ADDR_STR_LEN];
		int index;

		/* The label belonged to that pairing; keep it from resurfacing on a
		 * later bond that reuses the slot. */
		labels_ensure();
		bt_addr_to_str(&pending_addr.a, address, sizeof(address));
		index = label_find(pending_addr.type, address);
		if (index >= 0) {
			label_drop((size_t)index);
			(void)labels_save();
		}
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

bool linkr_bond_command(struct bt_conn *conn, const char *command, char *reply, size_t size)
{
	bool query = !strcmp(command, "bonds?");
	bool names = !strcmp(command, "bonds names?");
	bool rename = !strcmp(command, "bonds name") || !strncmp(command, "bonds name ", 11);
	bool remove = !strncmp(command, "bonds remove ", 13);
	if (!query && !names && !rename && !remove) return false;
	struct bt_conn_info info;
	if (!conn || bt_conn_get_info(conn, &info) || bt_conn_get_security(conn) < BT_SECURITY_L2) {
		snprintk(reply, size, "ERR bonds encrypted BLE required\n");
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
	} else if (names) {
		int n = snprintk(reply, size, "OK bonds names\n");

		labels_ensure();
		labels_prune(info.id);
		if (n < 0 || (size_t)n >= size) {
			snprintk(reply, size, "ERR bonds buffer\n");
		} else {
			size_t used = (size_t)n;

			for (size_t i = 0; i < label_blob[1]; i++) {
				const uint8_t *slot = label_slot(i);
				int written = name_line(reply + used, size - used, slot[0],
							(const char *)(slot + 1U),
							(const char *)(slot + 2U + LABEL_ADDR));

				if (written < 0 || (size_t)written >= size - used) {
					snprintk(reply, size, "ERR bonds buffer\n");
					break;
				}
				used += (size_t)written;
			}
		}
	} else if (rename) {
		const char *value = command[10] == ' ' ? command + 11 : "";
		const bt_addr_le_t *dst = bt_conn_get_dst(conn);
		char address[BT_ADDR_STR_LEN];

		/* Keyed exactly like self= above, so a label and the bond it belongs to
		 * always agree on which address identifies the pairing. A label with no
		 * bond behind it would be pruned on the next listing. */
		bt_addr_to_str(&dst->a, address, sizeof(address));
		if (!bt_le_bond_exists(info.id, dst)) {
			snprintk(reply, size, "ERR bonds not found\n");
		} else {
			int err;

			labels_ensure();
			err = label_set(dst->type, address, value);
			if (err == -EINVAL) {
				snprintk(reply, size, "ERR bonds name length\n");
			} else if (err == -ENOSPC) {
				snprintk(reply, size, "ERR bonds name slots\n");
			} else if (err != 0) {
				snprintk(reply, size, "ERR bonds name=%d\n", err);
			} else {
				snprintk(reply, size, "OK bonds name\n");
			}
		}
	} else {
		const char *value = command + 13;
		bt_addr_le_t address;
		/* Exact identity address, never a shifting list index or wildcard. */
		if (strlen(value) != 19 || (value[0] != '0' && value[0] != '1') || value[1] != '/' ||
		    bt_addr_le_from_str(value + 2, value[0] == '0' ? "public" : "random", &address)) {
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
