/* SPDX-License-Identifier: Apache-2.0 */
#include "ble_nus.h"

/* The standard NUS instance uses plaintext permissions. Instantiate the same
 * wire service with encryption on RX, TX and CCC, retaining upstream callback
 * dispatch without modifying the Zephyr checkout. */
BT_GATT_SERVICE_DEFINE(linkr_nus_service,
	BT_GATT_PRIMARY_SERVICE(BT_UUID_NUS_SERVICE),
	BT_GATT_CHARACTERISTIC(BT_UUID_NUS_TX_CHAR, BT_GATT_CHRC_NOTIFY,
		BT_GATT_PERM_READ_ENCRYPT, NULL, NULL, NULL),
	BT_GATT_CCC(nus_ccc_cfg_changed,
		BT_GATT_PERM_READ_ENCRYPT | BT_GATT_PERM_WRITE_ENCRYPT),
	BT_GATT_CHARACTERISTIC(BT_UUID_NUS_RX_CHAR,
		BT_GATT_CHRC_WRITE | BT_GATT_CHRC_WRITE_WITHOUT_RESP,
		BT_GATT_PERM_WRITE_ENCRYPT, NULL, nus_bt_chr_write, NULL)
);

static sys_slist_t linkr_nus_callbacks = SYS_SLIST_STATIC_INIT(&linkr_nus_callbacks);
STRUCT_SECTION_ITERABLE(bt_nus_inst, linkr_nus) = {
	.svc = &linkr_nus_service,
	.cbs = &linkr_nus_callbacks,
};
