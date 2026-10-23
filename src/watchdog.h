/* SPDX-License-Identifier: Apache-2.0 */
#ifndef LINKR_WATCHDOG_H_
#define LINKR_WATCHDOG_H_

/* Each channel has one owner. An idle owner must still visit its bounded wait. */
enum linkr_watchdog_channel {
	LINKR_WDT_STARTUP,
	LINKR_WDT_SYSWORK,
	LINKR_WDT_UART_RX,
	LINKR_WDT_UART_TX,
	LINKR_WDT_MGMT_REQUEST,
	LINKR_WDT_MGMT_TX,
	LINKR_WDT_CHANNEL_COUNT,
};

#ifdef CONFIG_LINKR_BLE_BRIDGE_WATCHDOG
void linkr_watchdog_feed(enum linkr_watchdog_channel channel);
void linkr_watchdog_boot_complete(void);
#else
static inline void linkr_watchdog_feed(enum linkr_watchdog_channel channel)
{
	(void)channel;
}
static inline void linkr_watchdog_boot_complete(void) {}
#endif

#endif
