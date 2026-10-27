/* SPDX-License-Identifier: Apache-2.0 */
#include <errno.h>
#include <string.h>
#include <zephyr/bluetooth/gatt.h>
#include <zephyr/bluetooth/hci.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/util.h>
#include "ble_nus.h"
#include "ble_mgmt.h"
#include "ble_peers.h"
#include "ble_uart_reliable.h"
#include "serial_session.h"
#include "watchdog.h"

#define QUEUE_DEPTH CONFIG_LINKR_BLE_BRIDGE_PEER_TX_QUEUE_DEPTH
#define PAYLOAD_SIZE LINKR_UART_RELIABLE_MAX_PAYLOAD
struct peer_packet { uint32_t generation; uint16_t len; uint8_t data[PAYLOAD_SIZE]; };
static struct {
	struct bt_conn *conn;
	bool closing;
	uint32_t generation;
	struct k_msgq queue;
	char storage[QUEUE_DEPTH * sizeof(struct peer_packet)] __aligned(4);
	struct k_thread thread;
} peers[CONFIG_BT_MAX_CONN];
K_MUTEX_DEFINE(peers_lock);
K_THREAD_STACK_ARRAY_DEFINE(peer_stacks, CONFIG_BT_MAX_CONN, 2048);

static void peer_tx(void *arg, void *unused1, void *unused2)
{
	size_t index = (size_t)arg;
	struct peer_packet packet;
	ARG_UNUSED(unused1);
	ARG_UNUSED(unused2);
	for (;;) {
		struct bt_conn *conn = NULL;
		int err = 0;
		if (index == 0) linkr_watchdog_feed(LINKR_WDT_MGMT_TX);
		k_mutex_lock(&peers_lock, K_FOREVER);
		if (peers[index].conn && !peers[index].closing) conn = bt_conn_ref(peers[index].conn);
		k_mutex_unlock(&peers_lock);
		if (conn) {
			linkr_mgmt_tx_poll(conn);
			bt_conn_unref(conn);
			conn = NULL;
		}
		if (k_msgq_get(&peers[index].queue, &packet, K_MSEC(10))) continue;
		k_mutex_lock(&peers_lock, K_FOREVER);
		if (peers[index].conn && !peers[index].closing &&
		    packet.generation == peers[index].generation) conn = bt_conn_ref(peers[index].conn);
		k_mutex_unlock(&peers_lock);
		if (!conn) continue;
		if (linkr_uart_reliable_mode(conn)) err = linkr_uart_reliable_send(conn, packet.data, packet.len);
		else if (linkr_nus_subscribed(conn)) {
			uint16_t mtu = bt_gatt_get_mtu(conn);
			uint16_t limit = MIN(mtu > 3 ? mtu - 3 : 1, CONFIG_BT_BUF_ACL_TX_SIZE - 7);
			for (size_t sent = 0; sent < packet.len && !err;) {
				uint16_t count = MIN(packet.len - sent, limit);
				for (int retry = 0; retry < 20; retry++) {
					err = linkr_nus_send(conn, packet.data + sent, count);
					if (err != -ENOMEM && err != -EAGAIN) break;
					k_sleep(K_MSEC(10));
				}
				sent += count;
			}
		}
		/* Close slow peers instead of silently corrupting their stream. */
		if (err && err != -ENOTCONN) bt_conn_disconnect(conn, BT_HCI_ERR_REMOTE_USER_TERM_CONN);
		bt_conn_unref(conn);
	}
}

void linkr_ble_peers_init(void)
{
	for (size_t i = 0; i < ARRAY_SIZE(peers); i++) {
		k_msgq_init(&peers[i].queue, peers[i].storage, sizeof(struct peer_packet), QUEUE_DEPTH);
		k_thread_create(&peers[i].thread, peer_stacks[i], K_THREAD_STACK_SIZEOF(peer_stacks[i]),
				peer_tx, (void *)i, NULL, NULL, 7, 0, K_NO_WAIT);
		k_thread_name_set(&peers[i].thread, "ble_peer_tx");
	}
}

int linkr_ble_peer_connected(struct bt_conn *conn)
{
	int err = linkr_session_open(conn, LINKR_SESSION_BLE);
	if (err) return err;
	k_mutex_lock(&peers_lock, K_FOREVER);
	size_t i = bt_conn_index(conn);
	k_msgq_purge(&peers[i].queue);
	peers[i].conn = bt_conn_ref(conn);
	peers[i].generation++;
	peers[i].closing = false;
	k_mutex_unlock(&peers_lock);
	return 0;
}

bool linkr_ble_peer_disconnected(struct bt_conn *conn)
{
	bool attached = false;
	linkr_session_close(conn);
	k_mutex_lock(&peers_lock, K_FOREVER);
	size_t i = bt_conn_index(conn);
	if (peers[i].conn == conn) {
		attached = true;
		bt_conn_unref(peers[i].conn);
		peers[i].conn = NULL;
		peers[i].closing = true;
		k_msgq_purge(&peers[i].queue);
	}
	k_mutex_unlock(&peers_lock);
	return attached;
}

int linkr_ble_feed(const uint8_t *data, size_t len)
{
	int result = -ENOTCONN;
	while (len) {
		struct peer_packet packet = { .len = MIN(len, PAYLOAD_SIZE) };
		memcpy(packet.data, data, packet.len);
		k_mutex_lock(&peers_lock, K_FOREVER);
		for (size_t i = 0; i < ARRAY_SIZE(peers); i++) {
			struct bt_conn *conn = peers[i].conn;
			if (!conn || peers[i].closing || bt_conn_get_security(conn) < BT_SECURITY_L2 ||
			    (!linkr_uart_reliable_mode(conn) && !linkr_nus_subscribed(conn))) continue;
			packet.generation = peers[i].generation;
			if (k_msgq_put(&peers[i].queue, &packet, K_NO_WAIT)) {
				if (result) result = -ENOMEM;
				peers[i].closing = true;
				bt_conn_disconnect(conn, BT_HCI_ERR_REMOTE_USER_TERM_CONN);
			} else result = 0;
		}
		k_mutex_unlock(&peers_lock);
		data += packet.len;
		len -= packet.len;
	}
	return result;
}
