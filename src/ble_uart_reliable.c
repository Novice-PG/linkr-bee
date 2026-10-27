/*
 * Linkr Reliable UART Service v1.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include <zephyr/bluetooth/gatt.h>
#include <zephyr/bluetooth/hci.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/atomic.h>
#include <zephyr/sys/byteorder.h>

#include "ble_uart_reliable.h"
#include "serial_session.h"

LOG_MODULE_REGISTER(linkr_ble_uart_reliable, LOG_LEVEL_INF);

#define RX_TIMEOUT_MS 2000
#define INDICATE_TIMEOUT_MS 5000
#define FRAGMENT_MAX 244

struct reliable_header {
	uint8_t magic[2];
	uint8_t version;
	uint8_t flags;
	uint8_t sequence_le[4];
	uint8_t payload_len_le[2];
	uint8_t reserved[2];
} __packed;

struct reliable_state_value {
	uint8_t version;
	uint8_t flags;
	uint8_t max_payload_le[2];
	uint8_t rx_next_sequence_le[4];
	uint8_t tx_next_sequence_le[4];
	uint8_t tx_acked_sequence_le[4];
} __packed;

BUILD_ASSERT(sizeof(struct reliable_header) == LINKR_UART_RELIABLE_HEADER_SIZE);
BUILD_ASSERT(sizeof(struct reliable_state_value) == 16);

static linkr_uart_reliable_write_fn uart_write_fn;
static struct k_mutex rx_lock;
static struct reliable_peer {
	struct {
		struct bt_conn *conn;
		uint32_t ticket;
		uint32_t sequence;
		uint16_t expected;
		uint16_t received;
		int64_t started_at;
		uint8_t payload[LINKR_UART_RELIABLE_MAX_PAYLOAD];
	} rx;
	uint32_t rx_next;
	uint32_t tx_next;
	uint32_t tx_acked;
	atomic_t selected;
	struct k_sem done;
	struct bt_gatt_indicate_params params;
	int result;
} reliable_peers[CONFIG_BT_MAX_CONN];

/*
 * Instrumentation for the confirmed-indication handoff.
 *
 * Question it was added to answer: do the 40-92 ms per-fragment handoff stalls
 * come from this module's own 25 ms retry backoff (`slept_ms`), or from waiting
 * on the peer (`wait_max_ms`)? The counters answer by measurement rather than
 * assumption and change no control flow.
 *
 * Measured outcome (2026-10-12, diagnostic firmware, 16 KiB uplink):
 *   ind=254 retry=0 busy=0 nomem=0 slept_ms=0 timeout=0 wait_max_ms=123
 * The backoff never fired — not once in 300 indications. So the bimodal
 * 30 ms / 60 ms split is NOT the retry sleep; the stall is the confirmed
 * indication round trip itself, serialised against a 30 ms connection interval
 * (air capture: Write Req -> Write Resp took 54.7 ms, and adjacent indications
 * landed 30.0 ms apart). Kept in the tree because it re-answers the same
 * question after any future change to the handoff path, at the cost of a few
 * atomics.
 */
static atomic_t diag_ind_total;
static atomic_t diag_ind_retry;
static atomic_t diag_ind_busy;
static atomic_t diag_ind_nomem;
static atomic_t diag_ind_slept_ms;
static atomic_t diag_ind_timeout;
static atomic_t diag_ind_wait_max_ms;

void linkr_uart_reliable_diag(uint32_t *total, uint32_t *retry, uint32_t *busy,
			      uint32_t *nomem, uint32_t *slept_ms,
			      uint32_t *timeout, uint32_t *wait_max_ms)
{
	if (total) {
		*total = (uint32_t)atomic_get(&diag_ind_total);
	}
	if (retry) {
		*retry = (uint32_t)atomic_get(&diag_ind_retry);
	}
	if (busy) {
		*busy = (uint32_t)atomic_get(&diag_ind_busy);
	}
	if (nomem) {
		*nomem = (uint32_t)atomic_get(&diag_ind_nomem);
	}
	if (slept_ms) {
		*slept_ms = (uint32_t)atomic_get(&diag_ind_slept_ms);
	}
	if (timeout) {
		*timeout = (uint32_t)atomic_get(&diag_ind_timeout);
	}
	if (wait_max_ms) {
		*wait_max_ms = (uint32_t)atomic_get(&diag_ind_wait_max_ms);
	}
}

/* Keep the largest observed wait without a lock: read, compare, retry on a
 * lost race. The value is a diagnostic, so a torn update is not a concern as
 * long as the maximum never goes backwards. */
static void diag_note_max(atomic_t *slot, uint32_t value)
{
	uint32_t current = (uint32_t)atomic_get(slot);

	while (value > current) {
		if (atomic_cas(slot, current, value)) {
			break;
		}
		current = (uint32_t)atomic_get(slot);
	}
}

static uint32_t sequence_next(uint32_t sequence)
{
	return sequence == UINT32_MAX ? 1 : sequence + 1;
}

static void rx_reset_locked(struct bt_conn *conn)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	if (peer->rx.conn) {
		bt_conn_unref(peer->rx.conn);
	}
	memset(&peer->rx, 0, sizeof(peer->rx));
}

static ssize_t state_read(struct bt_conn *conn,
			  const struct bt_gatt_attr *attr, void *buf,
			  uint16_t len, uint16_t offset)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	struct reliable_state_value state = {
		.version = LINKR_UART_RELIABLE_API_MAJOR,
		.flags = linkr_uart_reliable_mode(conn) ? 1 : 0,
		.max_payload_le = {
			LINKR_UART_RELIABLE_MAX_PAYLOAD & 0xff,
			LINKR_UART_RELIABLE_MAX_PAYLOAD >> 8,
		},
	};

	ARG_UNUSED(attr);
	sys_put_le32(peer->rx_next, state.rx_next_sequence_le);
	sys_put_le32(peer->tx_next, state.tx_next_sequence_le);
	sys_put_le32(peer->tx_acked, state.tx_acked_sequence_le);
	return bt_gatt_attr_read(conn, attr, buf, len, offset, &state,
				 sizeof(state));
}

static ssize_t rx_write(struct bt_conn *conn,
			const struct bt_gatt_attr *attr, const void *buf,
			uint16_t len, uint16_t offset, uint8_t flags)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	const uint8_t *bytes = buf;
	const struct reliable_header *header;
	uint16_t fragment_len;
	uint32_t previous_sequence = peer->rx_next == 1 ?
				     UINT32_MAX : peer->rx_next - 1;
	int err = 0;

	ARG_UNUSED(attr);
	if (offset != 0 || (flags & BT_GATT_WRITE_FLAG_PREPARE)) {
		return BT_GATT_ERR(BT_ATT_ERR_NOT_SUPPORTED);
	}

	k_mutex_lock(&rx_lock, K_FOREVER);
	/* Ownership changes abandon an old partial frame before the next owner
	 * starts a frame; never infer framing from bytes inside an active epoch. */
	if (peer->rx.expected && peer->rx.ticket != linkr_session_ticket(conn, false)) {
		rx_reset_locked(conn);
	}
	uint32_t ticket = peer->rx.expected ? peer->rx.ticket : linkr_session_ticket(conn, true);
	if (!linkr_session_begin(ticket)) {
		rx_reset_locked(conn);
		k_mutex_unlock(&rx_lock);
		return BT_GATT_ERR(BT_ATT_ERR_WRITE_NOT_PERMITTED);
	}
	if (peer->rx.expected &&
	    k_uptime_get() - peer->rx.started_at > RX_TIMEOUT_MS) {
		rx_reset_locked(conn);
	}

	if (!peer->rx.expected) {
		if (len < sizeof(*header)) {
			err = BT_GATT_ERR(BT_ATT_ERR_INVALID_ATTRIBUTE_LEN);
			goto out;
		}
		header = (const struct reliable_header *)bytes;
		fragment_len = len - sizeof(*header);
		if (header->magic[0] != 'L' || header->magic[1] != 'R' ||
		    header->version != LINKR_UART_RELIABLE_API_MAJOR ||
		    sys_get_le16(header->payload_len_le) == 0 ||
		    sys_get_le16(header->payload_len_le) >
			    LINKR_UART_RELIABLE_MAX_PAYLOAD ||
		    fragment_len > sys_get_le16(header->payload_len_le)) {
			err = BT_GATT_ERR(BT_ATT_ERR_VALUE_NOT_ALLOWED);
			goto out;
		}
		peer->rx.conn = bt_conn_ref(conn);
		peer->rx.ticket = ticket;
		peer->rx.sequence = sys_get_le32(header->sequence_le);
		peer->rx.expected = sys_get_le16(header->payload_len_le);
		peer->rx.received = fragment_len;
		peer->rx.started_at = k_uptime_get();
		memcpy(peer->rx.payload, bytes + sizeof(*header), fragment_len);
	} else {
		uint16_t remaining = peer->rx.expected - peer->rx.received;

		if (peer->rx.conn != conn || len > remaining) {
			rx_reset_locked(conn);
			err = BT_GATT_ERR(BT_ATT_ERR_VALUE_NOT_ALLOWED);
			goto out;
		}
		memcpy(peer->rx.payload + peer->rx.received, bytes, len);
		peer->rx.received += len;
	}

	if (peer->rx.received == peer->rx.expected) {
		if (peer->rx.sequence == peer->rx_next) {
			err = uart_write_fn(conn, peer->rx.payload, peer->rx.expected);
			if (!err) {
				peer->rx_next = sequence_next(peer->rx_next);
			}
		} else if (peer->rx.sequence != previous_sequence) {
			err = -EILSEQ;
		}
		rx_reset_locked(conn);
		if (err) {
			err = BT_GATT_ERR(err == -ENOMEM ?
					  BT_ATT_ERR_INSUFFICIENT_RESOURCES :
					  BT_ATT_ERR_VALUE_NOT_ALLOWED);
		}
	}

out:
	k_mutex_unlock(&rx_lock);
	linkr_session_end();
	return err ? err : len;
}

static ssize_t tx_ccc_write(struct bt_conn *conn,
			    const struct bt_gatt_attr *attr, uint16_t value)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	ARG_UNUSED(attr);
	if (value == BT_GATT_CCC_INDICATE) atomic_set(&peer->selected, 1);
	else if (!value && atomic_get(&peer->selected) == 1) atomic_clear(&peer->selected);
	return sizeof(value);
}

BT_GATT_SERVICE_DEFINE(linkr_uart_reliable_service,
	BT_GATT_PRIMARY_SERVICE(BT_UUID_LINKR_UART_RELIABLE_SERVICE),
	BT_GATT_CHARACTERISTIC(BT_UUID_LINKR_UART_RELIABLE_RX,
			       BT_GATT_CHRC_WRITE, BT_GATT_PERM_WRITE_ENCRYPT,
			       NULL, rx_write, NULL),
	BT_GATT_CHARACTERISTIC(BT_UUID_LINKR_UART_RELIABLE_TX,
			       BT_GATT_CHRC_INDICATE, BT_GATT_PERM_READ_ENCRYPT,
			       NULL, NULL, NULL),
	BT_GATT_CCC_WITH_WRITE_CB(NULL, tx_ccc_write,
		    BT_GATT_PERM_READ_ENCRYPT | BT_GATT_PERM_WRITE_ENCRYPT),
	BT_GATT_CHARACTERISTIC(BT_UUID_LINKR_UART_RELIABLE_STATE,
			       BT_GATT_CHRC_READ, BT_GATT_PERM_READ_ENCRYPT,
			       state_read, NULL, NULL));

static void indication_complete(struct bt_conn *conn,
				struct bt_gatt_indicate_params *params,
				uint8_t err)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	ARG_UNUSED(params);
	peer->result = err ? -EIO : 0;
	/* params remains owned by the GATT stack until the destroy callback. */
}

static void indication_destroy(struct bt_gatt_indicate_params *params)
{
	struct reliable_peer *peer = CONTAINER_OF(params, struct reliable_peer, params);
	k_sem_give(&peer->done);
}

static int indicate_fragment(struct bt_conn *conn, const uint8_t *data,
			     uint16_t len)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	int err;
	int64_t started_at = k_uptime_get();

	if (!bt_gatt_is_subscribed(conn, &linkr_uart_reliable_service.attrs[4],
				   BT_GATT_CCC_INDICATE)) {
		return -ENOTCONN;
	}

	atomic_inc(&diag_ind_total);
	for (int retry = 0; retry < 20; retry++) {
		k_sem_reset(&peer->done);
		peer->result = -ETIMEDOUT;
		memset(&peer->params, 0, sizeof(peer->params));
		peer->params.attr = &linkr_uart_reliable_service.attrs[4];
		peer->params.func = indication_complete;
		peer->params.destroy = indication_destroy;
		peer->params.data = data;
		peer->params.len = len;
		err = bt_gatt_indicate(conn, &peer->params);
		if (err != -EBUSY && err != -ENOMEM) {
			break;
		}
		/* Count the reason before sleeping: the split between -EBUSY and
		 * -ENOMEM says whether the ATT handshake is still settling or the
		 * ACL pool is genuinely exhausted. */
		if (err == -EBUSY) {
			atomic_inc(&diag_ind_busy);
		} else {
			atomic_inc(&diag_ind_nomem);
		}
		atomic_inc(&diag_ind_retry);
		atomic_add(&diag_ind_slept_ms, 25);
		k_sleep(K_MSEC(25));
	}
	if (err) {
		return err;
	}
	if (k_sem_take(&peer->done, K_MSEC(INDICATE_TIMEOUT_MS))) {
		/* Do not return while Zephyr still owns peer->params. Force the
		 * connection down so GATT completes the request, then wait for destroy. */
		atomic_inc(&diag_ind_timeout);
		LOG_WRN("Reliable UART indication timed out; disconnecting peer");
		(void)bt_conn_disconnect(conn, BT_HCI_ERR_REMOTE_USER_TERM_CONN);
		(void)k_sem_take(&peer->done, K_FOREVER);
		return -ETIMEDOUT;
	}
	/* Wall time from submitting the indication to its confirmation landing.
	 * This includes any sleep the retry above added, so a max that keeps
	 * climbing while `slept_ms` stays flat means the stall is elsewhere. */
	diag_note_max(&diag_ind_wait_max_ms,
		      (uint32_t)(k_uptime_get() - started_at));
	return peer->result;
}

int linkr_uart_reliable_init(linkr_uart_reliable_write_fn write_fn)
{
	if (!write_fn) {
		return -EINVAL;
	}
	uart_write_fn = write_fn;
	k_mutex_init(&rx_lock);
	for (size_t i = 0; i < ARRAY_SIZE(reliable_peers); i++) {
		reliable_peers[i].rx_next = 1;
		reliable_peers[i].tx_next = 1;
		k_sem_init(&reliable_peers[i].done, 0, 1);
	}
	return 0;
}

void linkr_uart_reliable_disconnected(struct bt_conn *conn)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	k_mutex_lock(&rx_lock, K_FOREVER);
	if (peer->rx.conn == conn) {
		rx_reset_locked(conn);
	}
	k_mutex_unlock(&rx_lock);
	atomic_clear(&peer->selected);
}

void linkr_uart_reliable_connected(struct bt_conn *conn)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	peer->rx_next = 1;
	peer->tx_next = 1;
	peer->tx_acked = 0;
	atomic_clear(&peer->selected);
}

bool linkr_uart_reliable_mode(struct bt_conn *conn)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	int selected = atomic_get(&peer->selected);
	return selected != 2 && bt_gatt_is_subscribed(conn,
		&linkr_uart_reliable_service.attrs[4], BT_GATT_CCC_INDICATE);
}

void linkr_uart_reliable_nus_selected(struct bt_conn *conn)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	atomic_set(&peer->selected, 2);
}

int linkr_uart_reliable_send(struct bt_conn *conn, const uint8_t *data,
			     uint16_t len)
{
	struct reliable_peer *peer = &reliable_peers[bt_conn_index(conn)];
	uint8_t fragment[FRAGMENT_MAX];
	struct reliable_header *header = (struct reliable_header *)fragment;
	uint16_t mtu_payload;
	uint16_t first_payload;
	uint16_t sent;
	int err;

	if (!conn || !data || !len || len > LINKR_UART_RELIABLE_MAX_PAYLOAD) {
		return -EINVAL;
	}
	uint16_t mtu = bt_gatt_get_mtu(conn);

	/* Clamp before subtracting: an unexchanged MTU reads back as 0 and
	 * `0 - 3` in uint16_t wraps to 65533, which the MIN below would then
	 * carry into the fragment size. */
	mtu_payload = MIN(mtu > 3 ? mtu - 3 : 1, (uint16_t)sizeof(fragment));
	if (mtu_payload <= sizeof(*header)) {
		return -EMSGSIZE;
	}

	header->magic[0] = 'L';
	header->magic[1] = 'R';
	header->version = LINKR_UART_RELIABLE_API_MAJOR;
	header->flags = 0;
	sys_put_le32(peer->tx_next, header->sequence_le);
	sys_put_le16(len, header->payload_len_le);
	header->reserved[0] = 0;
	header->reserved[1] = 0;
	first_payload = MIN(len, (uint16_t)(mtu_payload - sizeof(*header)));
	memcpy(fragment + sizeof(*header), data, first_payload);
	err = indicate_fragment(conn, fragment, sizeof(*header) + first_payload);
	if (err) {
		return err;
	}

	for (sent = first_payload; sent < len;) {
		uint16_t chunk = MIN((uint16_t)(len - sent), mtu_payload);

		err = indicate_fragment(conn, data + sent, chunk);
		if (err) {
			/* The header already promised the peer len bytes, so giving up
			 * here leaves its reassembler waiting forever. Fail the
			 * connection the way the indication timeout above does, so the
			 * peer reconnects instead of hanging. */
			LOG_WRN("Reliable UART indication continuation failed: %d; "
				"disconnecting peer", err);
			(void)bt_conn_disconnect(conn,
						 BT_HCI_ERR_REMOTE_USER_TERM_CONN);
			return err;
		}
		sent += chunk;
	}
	peer->tx_acked = peer->tx_next;
	peer->tx_next = sequence_next(peer->tx_next);
	return 0;
}
