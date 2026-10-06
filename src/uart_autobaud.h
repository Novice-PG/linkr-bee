/*
 * BACKLOG M5 — follow the peer's UART line rate instead of demanding it match.
 *
 * This lives in its own translation unit on purpose: src/main.c, Kconfig and
 * CMakeLists.txt are compiled by every board in the project (esp32, esp32c3,
 * esp32c5), and a few hundred lines of Espressif register work sitting in there
 * would be a standing merge conflict for anything else that touches the
 * bridge. Disable CONFIG_LINKR_BLE_BRIDGE_UART_AUTOBAUD and this whole header
 * collapses to no-op inline stubs; the file compiles to nothing.
 */
#ifndef LINKR_UART_AUTOBAUD_H_
#define LINKR_UART_AUTOBAUD_H_

#include <stddef.h>
#include <stdint.h>

#include <zephyr/sys/util.h>

/*
 * The loopback test config drives the port from the test harness itself, so
 * there is no peer on the other end to follow and no rate to look for.
 */
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_UART_AUTOBAUD) && \
	!IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_TEST_UART_LOOPBACK_VERIFY)
#define LINKR_UART_AUTOBAUD 1
#else
#define LINKR_UART_AUTOBAUD 0
#endif

#if LINKR_UART_AUTOBAUD

/*
 * Provided by src/main.c, which owns the port's configuration. Called from the
 * system workqueue only — never from the UART interrupt.
 */
uint32_t linkr_uart_config_current(void);
int linkr_uart_config_reconfigure(uint32_t baudrate);

/**
 * Arm the hardware pulse-width detector and start following the peer's line
 * rate. Must be called once, after the port has been configured.
 *
 * @param reg_addr MMIO base of the bridge UART, as DT_REG_ADDR().
 */
void linkr_uart_autobaud_init(uintptr_t reg_addr);

#else

static inline uint32_t linkr_uart_config_current(void)
{
	return 0;
}

static inline int linkr_uart_config_reconfigure(uint32_t baudrate)
{
	(void)baudrate;

	return 0;
}

static inline void linkr_uart_autobaud_init(uintptr_t reg_addr)
{
	(void)reg_addr;
}

#endif /* LINKR_UART_AUTOBAUD */

#endif /* LINKR_UART_AUTOBAUD_H_ */
