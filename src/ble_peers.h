/* SPDX-License-Identifier: Apache-2.0 */
#ifndef LINKR_BLE_PEERS_H
#define LINKR_BLE_PEERS_H
#include <stddef.h>
#include <stdint.h>
#include <zephyr/bluetooth/conn.h>
void linkr_ble_peers_init(void);
int linkr_ble_peer_connected(struct bt_conn *conn);
bool linkr_ble_peer_disconnected(struct bt_conn *conn);
/* Returns 0 if at least one subscriber accepted data into its output queue. */
int linkr_ble_feed(const uint8_t *data, size_t len);
/* Bytes discarded so far because a peer's output queue was full. */
uint32_t linkr_ble_peer_dropped(void);
#endif
