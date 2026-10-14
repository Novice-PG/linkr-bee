/*
 * Follow the peer's UART line rate instead of demanding that it match ours.
 *
 * The port is configured once from CONFIG_LINKR_BLE_BRIDGE_UART_BAUD_RATE and
 * otherwise never questioned, while that same Kconfig's help lists 1500000 /
 * 921600 / 1000000 as typical SBC console rates. Swap the SBC and the bridge
 * goes deaf with nothing in the firmware noticing. "Linkr adapts to the peer"
 * is the direction that has to hold: the board being debugged must not be
 * asked to change a single line of its configuration to suit us.
 *
 * This lives in its own translation unit because src/main.c, Kconfig and
 * CMakeLists.txt are compiled for every board in the project; several hundred
 * lines of Espressif register work sitting in there would be a standing merge
 * conflict for anything else that touches the bridge. Build with
 * CONFIG_LINKR_BLE_BRIDGE_UART_AUTOBAUD=n and this header collapses to no-op
 * inline stubs, and the file compiles to nothing.
 */
#ifndef LINKR_UART_AUTOBAUD_H_
#define LINKR_UART_AUTOBAUD_H_

#include <stdint.h>

#include <zephyr/sys/util.h>

/*
 * The loopback verification config drives the port from the test harness, so
 * there is no peer to follow: one of us has to be the fixed end, and it should
 * be the harness.
 */
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_UART_AUTOBAUD) &&                      \
	!IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_TEST_UART_LOOPBACK_VERIFY)
#define LINKR_UART_AUTOBAUD 1
#else
#define LINKR_UART_AUTOBAUD 0
#endif

#if LINKR_UART_AUTOBAUD

/*
 * Both are provided by src/main.c, which owns the port's configuration and is
 * the only thing that knows how it is locked. Called from the system workqueue
 * only — never from the UART interrupt, because reconfiguring the port resets
 * the RX ring and must not race the consumer that reads it.
 */

/**
 * The line rate the port is configured for now, or 0 if that is not known.
 */
uint32_t linkr_uart_config_current(void);

/**
 * Move the port to `baudrate`, leaving framing alone.
 *
 * @retval 0 The port moved; the caller may assume subsequent traffic arrives
 *         at `baudrate`.
 * @retval negative The port did not move. Nothing has been lost; a later
 *         window will be measured and tried again.
 */
int linkr_uart_config_reconfigure(uint32_t baudrate);

/**
 * Stand down for a while after the rate was set deliberately.
 *
 * Detection exists so the peer does not have to be configured to match us.
 * It is not supposed to overrule a person: somebody typing a `@u=` command is
 * usually reading a board with a known rate and would rather the port stayed
 * put for a good few seconds than be "helpfully" moved underneath them. After
 * the hold expires detection resumes and, if the peer really is elsewhere,
 * takes over again.
 */
void linkr_uart_autobaud_suspend(void);

/**
 * Arm the hardware pulse-width detector and begin following the peer.
 *
 * Called once, after the port has been configured.
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

static inline void linkr_uart_autobaud_suspend(void)
{
}

static inline void linkr_uart_autobaud_init(uintptr_t reg_addr)
{
	(void)reg_addr;
}

#endif /* LINKR_UART_AUTOBAUD */

#endif /* LINKR_UART_AUTOBAUD_H_ */
