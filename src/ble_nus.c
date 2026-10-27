/* SPDX-License-Identifier: Apache-2.0 */
#include "ble_nus.h"
#include "ble_uart_reliable.h"
#include "serial_session.h"

static ssize_t nus_rx_write(struct bt_conn *conn, const struct bt_gatt_attr *attr,
			    const void *buf, uint16_t len, uint16_t offset, uint8_t flags)
{
	uint32_t ticket = linkr_session_ticket(conn, true);
	if (!linkr_session_begin(ticket)) return BT_GATT_ERR(BT_ATT_ERR_WRITE_NOT_PERMITTED);
	ssize_t result = nus_bt_chr_write(conn, attr, buf, len, offset, flags);
	linkr_session_end();
	return result;
}

static ssize_t nus_ccc_write(struct bt_conn *conn,
			     const struct bt_gatt_attr *attr, uint16_t value)
{
	ARG_UNUSED(attr);
	if (value == BT_GATT_CCC_NOTIFY) linkr_uart_reliable_nus_selected(conn);
	return sizeof(value);
}

bool linkr_nus_subscribed(struct bt_conn *conn)
{
	return bt_gatt_is_subscribed(conn, &linkr_nus.svc->attrs[2], BT_GATT_CCC_NOTIFY);
}

/* The standard NUS instance uses plaintext permissions. Instantiate the same
 * wire service with encryption on RX, TX and CCC, retaining upstream callback
 * dispatch without modifying the Zephyr checkout. */
BT_GATT_SERVICE_DEFINE(linkr_nus_service,
	BT_GATT_PRIMARY_SERVICE(BT_UUID_NUS_SERVICE),
	BT_GATT_CHARACTERISTIC(BT_UUID_NUS_TX_CHAR, BT_GATT_CHRC_NOTIFY,
		BT_GATT_PERM_READ_ENCRYPT, NULL, NULL, NULL),
	BT_GATT_CCC_WITH_WRITE_CB(nus_ccc_cfg_changed, nus_ccc_write,
		BT_GATT_PERM_READ_ENCRYPT | BT_GATT_PERM_WRITE_ENCRYPT),
	BT_GATT_CHARACTERISTIC(BT_UUID_NUS_RX_CHAR,
		BT_GATT_CHRC_WRITE | BT_GATT_CHRC_WRITE_WITHOUT_RESP,
		BT_GATT_PERM_WRITE_ENCRYPT, NULL, nus_rx_write, NULL)
);

static sys_slist_t linkr_nus_callbacks = SYS_SLIST_STATIC_INIT(&linkr_nus_callbacks);
STRUCT_SECTION_ITERABLE(bt_nus_inst, linkr_nus) = {
	.svc = &linkr_nus_service,
	.cbs = &linkr_nus_callbacks,
};
