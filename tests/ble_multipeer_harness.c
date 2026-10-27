/* SPDX-License-Identifier: Apache-2.0 */
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "ble_mgmt.h"
#include "ble_uart_reliable.h"
#define __packed __attribute__((packed))
#define BUILD_ASSERT(x) _Static_assert(x, #x)
#define __aligned(x) __attribute__((aligned(x)))
#define LINKR_MGMT_TX_QUEUE_DEPTH 4
#define ARG_UNUSED(x) (void)(x)
#define K_FOREVER 0
#define K_NO_WAIT 0
#define BT_GATT_WRITE_FLAG_PREPARE 1
#define BT_GATT_CCC_INDICATE 2
#define BT_ATT_ERR_NOT_SUPPORTED 6
#define BT_ATT_ERR_INVALID_ATTRIBUTE_LEN 13
#define BT_ATT_ERR_VALUE_NOT_ALLOWED 19
#define BT_ATT_ERR_INSUFFICIENT_RESOURCES 17
#define BT_ATT_ERR_WRITE_NOT_PERMITTED 3
#define BT_GATT_ERR(x) (-(x))
#define RX_TIMEOUT_MS 2000
#define LINKR_MGMT_RX_TIMEOUT_MS 2000
typedef int atomic_t;
struct k_mutex { int unused; };
struct k_sem { int unused; };
struct bt_gatt_attr { int unused; };
struct bt_gatt_indicate_params { int unused; };
static struct { struct bt_gatt_attr attrs[10]; } linkr_uart_reliable_service;
static void k_mutex_lock(struct k_mutex *m, int timeout) { (void)m; (void)timeout; }
static void k_mutex_unlock(struct k_mutex *m) { (void)m; }
static int64_t k_uptime_get(void) { return 0; }
static int atomic_get(atomic_t *a) { return *a; }
static void atomic_set(atomic_t *a, int value) { *a = value; }
static void atomic_clear(atomic_t *a) { *a = 0; }
static uint16_t sys_get_le16(const uint8_t *p) { return p[0] | ((uint16_t)p[1] << 8); }
static uint32_t sys_get_le32(const uint8_t *p) { return sys_get_le16(p) | ((uint32_t)sys_get_le16(p + 2) << 16); }
static void sys_put_le16(uint16_t x, uint8_t *p) { p[0] = x; p[1] = x >> 8; }
static void sys_put_le32(uint32_t x, uint8_t *p) { sys_put_le16(x, p); sys_put_le16(x >> 16, p + 2); }
static unsigned int bt_conn_index(struct bt_conn *c) { return c->index; }
static struct bt_conn *bt_conn_ref(struct bt_conn *c) { c->refs++; return c; }
static void bt_conn_unref(struct bt_conn *c) { assert(c->refs > 1); c->refs--; }
static bool bt_gatt_is_subscribed(struct bt_conn *c, const struct bt_gatt_attr *attr, uint16_t value)
{ (void)attr; (void)value; return c->subscribed; }
static ssize_t bt_gatt_attr_read(struct bt_conn *c, const struct bt_gatt_attr *attr,
 void *out, uint16_t len, uint16_t offset, const void *data, uint16_t size)
{ (void)c; (void)attr; (void)offset; assert(len >= size); memcpy(out, data, size); return size; }
static uint32_t current_ticket = 1;
static uint32_t linkr_session_ticket(const void *peer, bool claim) { (void)peer; (void)claim; return current_ticket; }
static uint32_t linkr_session_epoch(const void *peer) { (void)peer; return 1; }
static bool linkr_session_begin(uint32_t ticket) { return ticket != 0; }
static void linkr_session_end(void) { }
struct k_msgq { int unused; };
static struct k_msgq request_queue;
static bool queue_full;
static int k_msgq_put(struct k_msgq *q, const void *message, int timeout);
static int k_msgq_get(struct k_msgq *q, void *message, int timeout);
struct linkr_mgmt_tx_message;
static struct linkr_mgmt_tx_message *tx_pending[2];
static int allocations;
static void *k_malloc(size_t size) { allocations++; return malloc(size); }
static void k_free(void *ptr) { allocations--; free(ptr); }

#include "multipeer_production.inc"

static struct bt_conn a = { .index = 0, .refs = 1, .subscribed = true };
static struct bt_conn b = { .index = 1, .refs = 1, .subscribed = true };
static uint8_t output[2][64];
static size_t received[2];
static int uart_error;
static int writer(struct bt_conn *conn, const uint8_t *data, size_t len)
{
 if (uart_error) return uart_error;
 assert(received[conn->index] + len <= sizeof(output[0]));
 memcpy(output[conn->index] + received[conn->index], data, len);
 received[conn->index] += len;
 return 0;
}
#ifndef TEST_RELIABLE
static struct linkr_mgmt_request queued[4];
static unsigned int queued_count;
#endif
static int k_msgq_get(struct k_msgq *q, void *message, int timeout)
{
 (void)message; (void)timeout; assert(q != &request_queue);
#ifndef TEST_RELIABLE
 for (unsigned int i = 0; i < 2; i++) {
  if (q == &tx_peers[i].queue && tx_pending[i]) {
   memcpy(message, &tx_pending[i], sizeof(tx_pending[i])); tx_pending[i] = NULL; return 0;
  }
 }
#endif
 return -ENOMSG;
}
static int k_msgq_put(struct k_msgq *q, const void *message, int timeout)
{
 (void)q; (void)message; (void)timeout;
 if (queue_full) return -ENOMEM;
#ifndef TEST_RELIABLE
 if (q == &request_queue) {
  assert(queued_count < 4);
  memcpy(&queued[queued_count++], message, sizeof(queued[0]));
 } else {
  for (unsigned int i = 0; i < 2; i++) {
   if (q == &tx_peers[i].queue) { assert(!tx_pending[i]); memcpy(&tx_pending[i], message, sizeof(tx_pending[i])); }
  }
 }
#endif
 return 0;
}

static size_t frame(uint8_t *out, const uint8_t *payload, size_t total, size_t first)
{
 memset(out, 0, 12);
 out[0] = 'L';
#ifdef TEST_RELIABLE
 out[1] = 'R';
#else
 out[1] = 'K'; out[3] = 1;
#endif
 out[2] = 1;
 sys_put_le32(1, out + 4); sys_put_le16(total, out + 8);
 memcpy(out + 12, payload, first);
 return 12 + first;
}

int main(int argc, char **argv)
{
 assert(argc == 2);
 uint8_t one[64], two[64];
 size_t len1 = frame(one, (const uint8_t *)"AAAA", 4, 2);
 size_t len2 = frame(two, (const uint8_t *)"BBBB", 4, 2);
#ifdef TEST_RELIABLE
 uart_write_fn = writer;
 reliable_peers[0].rx_next = reliable_peers[1].rx_next = 1;
 reliable_peers[0].tx_next = reliable_peers[1].tx_next = 1;
 if (!strcmp(argv[1], "interleave")) {
  assert(rx_write(&a, NULL, one, len1, 0, 0) == (int)len1);
  assert(rx_write(&b, NULL, two, len2, 0, 0) == (int)len2);
  assert(rx_write(&b, NULL, "BB", 2, 0, 0) == 2);
  linkr_uart_reliable_disconnected(&a);
  assert(received[0] == 0 && received[1] == 4 && !memcmp(output[1], "BBBB", 4));
  len1 = frame(one, (const uint8_t *)"AAAA", 4, 4);
  assert(rx_write(&a, NULL, one, len1, 0, 0) == (int)len1);
  assert(received[0] == 4 && reliable_peers[1].rx_next == 2);
 } else if (!strcmp(argv[1], "retry")) {
  len1 = frame(one, (const uint8_t *)"AAAA", 4, 4);
  uart_error = -ENOMEM;
  assert(rx_write(&a, NULL, one, len1, 0, 0) == -BT_ATT_ERR_INSUFFICIENT_RESOURCES);
  assert(reliable_peers[0].rx_next == 1 && !received[0]);
  uart_error = 0;
  assert(rx_write(&a, NULL, one, len1, 0, 0) == (int)len1);
  assert(reliable_peers[0].rx_next == 2 && received[0] == 4);
 } else if (!strcmp(argv[1], "binary")) {
  uint8_t payload[28] = {0};
  frame(payload + 8, (const uint8_t *)"12345678", 8, 8);
  len1 = frame(one, payload, sizeof(payload), 8);
  assert(rx_write(&a, NULL, one, len1, 0, 0) == (int)len1);
  assert(rx_write(&a, NULL, payload + 8, 20, 0, 0) == 20);
  assert(received[0] == 28 && !memcmp(output[0], payload, 28));
 } else if (!strcmp(argv[1], "handoff")) {
  assert(rx_write(&a, NULL, one, len1, 0, 0) == (int)len1);
  current_ticket = 2;
  len1 = frame(one, (const uint8_t *)"AAAA", 4, 4);
  assert(rx_write(&a, NULL, one, len1, 0, 0) == (int)len1);
  assert(received[0] == 4 && !memcmp(output[0], "AAAA", 4));
 } else if (!strcmp(argv[1], "transport")) {
  tx_ccc_write(&a, NULL, BT_GATT_CCC_INDICATE);
  linkr_uart_reliable_nus_selected(&b);
  assert(linkr_uart_reliable_mode(&a) && !linkr_uart_reliable_mode(&b));
  tx_ccc_write(&b, NULL, BT_GATT_CCC_INDICATE);
  linkr_uart_reliable_nus_selected(&a);
  assert(!linkr_uart_reliable_mode(&a) && linkr_uart_reliable_mode(&b));
 } else return 2;
#else
 if (!strcmp(argv[1], "late_response")) {
  linkr_mgmt_connected(&a); linkr_mgmt_connected(&b);
  assert(!linkr_mgmt_send(&a, LINKR_MGMT_MSG_RESPONSE, 1, 0, "A", 1));
  assert(!linkr_mgmt_send(&b, LINKR_MGMT_MSG_RESPONSE, 1, 0, "B", 1));
  linkr_mgmt_disconnected(&a);
  assert(a.refs == 1 && tx_pending[1]);
  assert(linkr_mgmt_send(&a, LINKR_MGMT_MSG_RESPONSE, 2, 0, "OLD", 3) == -ENOTCONN);
  assert(a.refs == 1 && tx_pending[1]);
  linkr_mgmt_disconnected(&b);
  assert(b.refs == 1 && allocations == 0);
  puts("ok"); return 0;
 }
 assert(!strcmp(argv[1], "interleave") || !strcmp(argv[1], "disconnect") || !strcmp(argv[1], "queue_full"));
 assert(command_write(&a, NULL, one, len1, 0, 0) == (int)len1);
 assert(command_write(&b, NULL, two, len2, 0, 0) == (int)len2);
 if (!strcmp(argv[1], "disconnect")) linkr_mgmt_disconnected(&a);
 else {
  queue_full = !strcmp(argv[1], "queue_full");
  assert(command_write(&a, NULL, "AA", 2, 0, 0) == (queue_full ? -BT_ATT_ERR_INSUFFICIENT_RESOURCES : 2));
  queue_full = false;
 }
 assert(command_write(&b, NULL, "BB", 2, 0, 0) == 2);
 assert(queued_count && queued[queued_count - 1].conn == &b);
 assert(queued[queued_count - 1].payload_len == 4 && !memcmp(queued[queued_count - 1].payload, "BBBB", 4));
 unsigned int before = queued_count;
 linkr_mgmt_disconnected(&a);
 assert(queued_count == before);
#endif
 puts("ok");
 return 0;
}
