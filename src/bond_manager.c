/* SPDX-License-Identifier: Apache-2.0 */
#include <errno.h>
#include <string.h>
#include <zephyr/bluetooth/bluetooth.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>
#include "bond_manager.h"

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
	if (!query && strncmp(command, "bonds remove ", 13)) return false;
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
