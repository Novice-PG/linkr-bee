/*
 * WS2812-compatible single-wire RGB status LED.
 *
 * Timing: 1.25 us per bit, high 350 ns for a 0 bit and 750 ns for a 1 bit.
 * The RISC-V cycle counter is the only clock precise enough for that on this
 * SoC (the Zephyr system clock runs far slower), so the frame is written with
 * interrupts locked and the pin driven through the GPIO W1TS/W1TC registers.
 * That is deliberate: the gpio_pin_set_dt() call path is longer than the
 * 350 ns high pulse.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_ws2812.h"

#include <errno.h>
#include <string.h>

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/gpio.h>
#include <zephyr/irq.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/util.h>

/* Shares main.c's log source: same prefix, no second source slot in RAM. */
LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

#if DT_HAS_ALIAS(linkr_rgb)

#include <esp_cpu.h>
#include <soc/gpio_struct.h>

#define LINKR_RGB_NODE DT_ALIAS(linkr_rgb)

/* The GPIO controller that owns the data pin, used for direct writes. */
static gpio_dev_t *const ws_regs = (gpio_dev_t *)DT_REG_ADDR(DT_NODELABEL(gpio0));
static const struct gpio_dt_spec ws_data =
	GPIO_DT_SPEC_GET(LINKR_RGB_NODE, data_gpios);
static const size_t ws_chain_length = (size_t)DT_PROP(LINKR_RGB_NODE, chain_length);

/*
 * The core clock is fixed by CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ (160 MHz on this
 * board). Use the Espressif CPU API for the core cycle counter: ESP32-C3
 * needs its chip-specific counter CSR; a direct `mcycle` read traps.
 * The Zephyr system clock (16 MHz) does not count CPU cycles.
 */
#define WS_CORE_HZ ((uint64_t)CONFIG_ESP_DEFAULT_CPU_FREQ_MHZ * 1000000ull)
#define WS_NS_TO_CYCLES(ns) ((uint32_t)((WS_CORE_HZ * (ns)) / 1000000000ull))
#define WS_T0H_CYCLES WS_NS_TO_CYCLES(350)
#define WS_T1H_CYCLES WS_NS_TO_CYCLES(750)
#define WS_TBIT_CYCLES WS_NS_TO_CYCLES(1250)
/* WS2812 latches after 50 us of idle; 300 us covers every clone. */
#define WS_RESET_US 300

static uint8_t ws_byte_order[3] = {1, 0, 2}; /* GRB, the WS2812 default */

static inline uint32_t ws_cycles(void)
{
	return esp_cpu_get_cycle_count();
}

static inline void ws_pin_high(void)
{
	ws_regs->out_w1ts.val = BIT(ws_data.pin);
}

static inline void ws_pin_low(void)
{
	ws_regs->out_w1tc.val = BIT(ws_data.pin);
}

/* One byte, MSB first, as 8 cells of 1.25 us. Caller holds the irq lock. */
static void ws_send_byte(uint8_t byte)
{
	for (int bit = 7; bit >= 0; bit--) {
		uint32_t start = ws_cycles();
		uint32_t high = (byte & BIT(bit)) ? WS_T1H_CYCLES : WS_T0H_CYCLES;

		ws_pin_high();
		while ((uint32_t)(ws_cycles() - start) < high) {
			/* spin: the high cell is a few dozen cycles */
		}
		ws_pin_low();
		while ((uint32_t)(ws_cycles() - start) < WS_TBIT_CYCLES) {
			/* spin: finish the 1.25 us cell */
		}
	}
}

static void ws_set_order(const char *order)
{
	if (order == NULL || strlen(order) != 3) {
		return;
	}

	for (int i = 0; i < 3; i++) {
		switch (order[i]) {
		case 'R':
			ws_byte_order[i] = 0;
			break;
		case 'G':
			ws_byte_order[i] = 1;
			break;
		case 'B':
			ws_byte_order[i] = 2;
			break;
		default:
			/* Keep the WS2812 default on an unknown channel. */
			ws_byte_order[0] = 1;
			ws_byte_order[1] = 0;
			ws_byte_order[2] = 2;
			return;
		}
	}
}

bool linkr_ws2812_available(void)
{
	return true;
}

int linkr_ws2812_init(void)
{
	int err;

	if (!device_is_ready(ws_data.port)) {
		return -ENODEV;
	}

	/* Configures the IO mux for plain GPIO output and enables the pad. */
	err = gpio_pin_configure_dt(&ws_data, GPIO_OUTPUT_INACTIVE);
	if (err) {
		return err;
	}

	ws_pin_low();
	ws_set_order(DT_PROP(LINKR_RGB_NODE, color_order));
	return 0;
}

int linkr_ws2812_write(const struct linkr_rgb *pixels, size_t count)
{
	unsigned int key;

	if (pixels == NULL || count == 0 || count > ws_chain_length) {
		return -EINVAL;
	}

	key = irq_lock();
	for (size_t i = 0; i < count; i++) {
		uint8_t channels[3] = {pixels[i].r, pixels[i].g, pixels[i].b};

		for (int byte = 0; byte < 3; byte++) {
			ws_send_byte(channels[ws_byte_order[byte]]);
		}
	}
	irq_unlock(key);

	/* Latch: the line must stay low well past the longest cell. */
	k_busy_wait(WS_RESET_US);
	return 0;
}

#else /* !DT_HAS_ALIAS(linkr_rgb) */

bool linkr_ws2812_available(void)
{
	return false;
}

int linkr_ws2812_init(void)
{
	return -ENODEV;
}

int linkr_ws2812_write(const struct linkr_rgb *pixels, size_t count)
{
	ARG_UNUSED(pixels);
	ARG_UNUSED(count);
	return -ENODEV;
}

#endif /* DT_HAS_ALIAS(linkr_rgb) */
