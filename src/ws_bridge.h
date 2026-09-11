/*
 * UART-over-WebSocket LAN bridge for the Linkr Bee bridge.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#ifndef LINKR_BLE_WS_BRIDGE_H
#define LINKR_BLE_WS_BRIDGE_H

#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <zephyr/sys/printk.h>
#include <zephyr/sys/util.h>

#ifdef __cplusplus
extern "C" {
#endif

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_WS_BRIDGE)

/* Register network management events and load the persisted enabled flag.
 * The HTTP/WebSocket server starts automatically once WiFi has an IP. */
int linkr_ws_init(void);

/* Runtime enable/disable; persisted to flash. Disabling stops the server
 * and closes any connected clients. */
int linkr_ws_set_enabled(bool enabled);
bool linkr_ws_is_enabled(void);

/* First-frame LAN auth token: 32 lowercase hex characters, or NULL/"" to
 * accept unauthenticated clients. Persisted. Returns -EINVAL on bad input. */
int linkr_ws_set_token(const char *token);

/* Mint and persist a fresh random token. Recovers a bridge that refused to
 * listen because no token was available at boot. */
int linkr_ws_rotate_token(void);

/* Feed UART RX bytes to all connected WebSocket clients. Called from the
 * uart_to_ble thread; never blocks on the network. No-op when disabled. */
void linkr_ws_feed(const uint8_t *data, size_t len);

/* Human-readable status for "@s?" (includes the token) and diagnostics for
 * "@i?" (deliberately excludes it). */
int linkr_ws_status(char *buf, size_t len);
int linkr_ws_diagnostics(char *buf, size_t len);

#else /* WS bridge compiled out: no-op stubs so main.c stays unchanged. */

static inline int  linkr_ws_init(void) { return 0; }
static inline int  linkr_ws_set_enabled(bool enabled) { (void)enabled; return -ENOTSUP; }
static inline bool linkr_ws_is_enabled(void) { return false; }
static inline int  linkr_ws_set_token(const char *token) { (void)token; return -ENOTSUP; }
static inline int  linkr_ws_rotate_token(void) { return -ENOTSUP; }
static inline void linkr_ws_feed(const uint8_t *data, size_t len) { (void)data; (void)len; }
static inline int  linkr_ws_status(char *buf, size_t len) { return snprintk(buf, len, "ws=disabled"); }
static inline int  linkr_ws_diagnostics(char *buf, size_t len) { return snprintk(buf, len, "state=disabled port=0 clients=0 tx=0 rx=0 dropped=0"); }

#endif

/* Implemented in main.c: atomically queue a complete write for bridge UART TX,
 * sharing the BLE RX path. Returns 0 on success, -ENOMEM without queuing a
 * partial prefix when capacity is insufficient. */
int linkr_uart_write(const uint8_t *data, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* LINKR_BLE_WS_BRIDGE_H */
