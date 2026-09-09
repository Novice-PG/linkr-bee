/* SPDX-License-Identifier: Apache-2.0 */
#ifndef LINKR_BLE_NUS_H
#define LINKR_BLE_NUS_H

#include <errno.h>
#include <zephyr/bluetooth/conn.h>
#include <zephyr/bluetooth/services/nus.h>

extern struct bt_nus_inst linkr_nus;

static inline int linkr_nus_send(struct bt_conn *conn, const void *data, uint16_t len)
{
	if (!conn || bt_conn_get_security(conn) < BT_SECURITY_L2) return -ENOTCONN;
	return bt_nus_inst_send(conn, &linkr_nus, data, len);
}

#endif
