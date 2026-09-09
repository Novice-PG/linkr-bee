/* SPDX-License-Identifier: Apache-2.0 */
#ifndef LINKR_BLE_SECURITY_H
#define LINKR_BLE_SECURITY_H

#include <zephyr/bluetooth/conn.h>

int linkr_ble_security_init(void);
void linkr_ble_security_connected(struct bt_conn *conn);

#endif
