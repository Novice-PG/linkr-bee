/*
 * Linkr Bee status LED: addressable RGB on the X1.0, plain GPIO elsewhere.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_status_led.h"

#include <errno.h>

#include <zephyr/devicetree.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/util.h>

#include "linkr_ws2812.h"

/* Shares main.c's log source: same prefix, no second source slot in RAM. */
LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

/* How long one activity flash stays visible. */
#define LINKR_ACTIVITY_MS 40

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_RGB_STATUS_LED) && DT_HAS_ALIAS(linkr_rgb)

/* ---------------------------------------------------------------------- */
/* Addressable RGB status LED (Linkr Bee X1.0)                            */
/* ---------------------------------------------------------------------- */

static struct linkr_rgb led_base;
static struct linkr_rgb led_activity_color = {
	.r = (CONFIG_LINKR_BLE_BRIDGE_RGB_ACTIVITY_COLOR >> 16) & 0xff,
	.g = (CONFIG_LINKR_BLE_BRIDGE_RGB_ACTIVITY_COLOR >> 8) & 0xff,
	.b = CONFIG_LINKR_BLE_BRIDGE_RGB_ACTIVITY_COLOR & 0xff,
};
static bool led_ready;
static bool led_activity_active;
static struct k_work_delayable led_apply_work;
static struct k_work_delayable led_activity_end_work;

static void led_apply(struct k_work *work)
{
	struct linkr_rgb color = led_activity_active ? led_activity_color : led_base;

	ARG_UNUSED(work);
	(void)linkr_ws2812_write(&color, 1);
}

static void led_activity_end(struct k_work *work)
{
	ARG_UNUSED(work);
	led_activity_active = false;
	(void)k_work_reschedule(&led_apply_work, K_NO_WAIT);
}

bool linkr_status_led_available(void)
{
	return linkr_ws2812_available();
}

int linkr_status_led_init(void)
{
	int err = linkr_ws2812_init();

	if (err) {
		return err;
	}

	k_work_init_delayable(&led_apply_work, led_apply);
	k_work_init_delayable(&led_activity_end_work, led_activity_end);
	led_base = (struct linkr_rgb){0, 0, 0};
	led_activity_active = false;
	led_ready = true;
	return 0;
}

void linkr_status_led_activity(void)
{
	if (!led_ready) {
		return;
	}

	led_activity_active = true;
	(void)k_work_reschedule(&led_apply_work, K_NO_WAIT);
	(void)k_work_reschedule(&led_activity_end_work, K_MSEC(LINKR_ACTIVITY_MS));
}

int linkr_status_led_set_rgb(uint8_t r, uint8_t g, uint8_t b)
{
	if (!led_ready) {
		return -ENODEV;
	}

	led_base = (struct linkr_rgb){.r = r, .g = g, .b = b};
	(void)k_work_reschedule(&led_apply_work, K_NO_WAIT);
	return 0;
}

void linkr_status_led_off(void)
{
	(void)linkr_status_led_set_rgb(0, 0, 0);
}

void linkr_status_led_get_rgb(uint8_t *r, uint8_t *g, uint8_t *b)
{
	if (r != NULL) {
		*r = led_base.r;
	}
	if (g != NULL) {
		*g = led_base.g;
	}
	if (b != NULL) {
		*b = led_base.b;
	}
}

void linkr_status_led_blink(int count, int delay_ms)
{
	if (!led_ready) {
		return;
	}

	for (int i = 0; i < count; i++) {
		(void)linkr_ws2812_write(&led_activity_color, 1);
		k_sleep(K_MSEC(delay_ms));
		(void)linkr_ws2812_write(&led_base, 1);
		k_sleep(K_MSEC(delay_ms));
	}
}

#elif DT_HAS_ALIAS(led0) && DT_NODE_HAS_STATUS(DT_ALIAS(led0), okay)

/* ---------------------------------------------------------------------- */
/* Single-colour GPIO status LED (dev boards, Super Mini)                  */
/* ---------------------------------------------------------------------- */

#include <zephyr/device.h>
#include <zephyr/drivers/gpio.h>

static const struct gpio_dt_spec led_gpio = GPIO_DT_SPEC_GET(DT_ALIAS(led0), gpios);
static struct k_work_delayable led_off_work;
static bool led_ready;
static bool led_on;

static void led_off_handler(struct k_work *work)
{
	ARG_UNUSED(work);
	gpio_pin_set_dt(&led_gpio, 0);
}

bool linkr_status_led_available(void)
{
	return true;
}

int linkr_status_led_init(void)
{
	if (!device_is_ready(led_gpio.port)) {
		return -ENODEV;
	}

	if (gpio_pin_configure_dt(&led_gpio, GPIO_OUTPUT_INACTIVE) != 0) {
		return -EIO;
	}

	k_work_init_delayable(&led_off_work, led_off_handler);
	led_ready = true;
	led_on = false;
	return 0;
}

void linkr_status_led_activity(void)
{
	if (!led_ready) {
		return;
	}

	gpio_pin_set_dt(&led_gpio, 1);
	(void)k_work_reschedule(&led_off_work, K_MSEC(LINKR_ACTIVITY_MS));
}

int linkr_status_led_set_rgb(uint8_t r, uint8_t g, uint8_t b)
{
	if (!led_ready) {
		return -ENODEV;
	}

	led_on = (r | g | b) != 0;
	gpio_pin_set_dt(&led_gpio, led_on ? 1 : 0);
	return 0;
}

void linkr_status_led_off(void)
{
	(void)linkr_status_led_set_rgb(0, 0, 0);
}

void linkr_status_led_get_rgb(uint8_t *r, uint8_t *g, uint8_t *b)
{
	/* The LED only has on/off state; report white so callers see "on". */
	uint8_t value = led_on ? 0xff : 0x00;

	if (r != NULL) {
		*r = value;
	}
	if (g != NULL) {
		*g = value;
	}
	if (b != NULL) {
		*b = value;
	}
}

void linkr_status_led_blink(int count, int delay_ms)
{
	if (!led_ready) {
		return;
	}

	for (int i = 0; i < count; i++) {
		gpio_pin_set_dt(&led_gpio, 1);
		k_sleep(K_MSEC(delay_ms));
		gpio_pin_set_dt(&led_gpio, 0);
		k_sleep(K_MSEC(delay_ms));
	}
}

#else

/* ---------------------------------------------------------------------- */
/* No status LED on this board                                            */
/* ---------------------------------------------------------------------- */

bool linkr_status_led_available(void)
{
	return false;
}

int linkr_status_led_init(void)
{
	return -ENODEV;
}

void linkr_status_led_activity(void)
{
}

int linkr_status_led_set_rgb(uint8_t r, uint8_t g, uint8_t b)
{
	ARG_UNUSED(r);
	ARG_UNUSED(g);
	ARG_UNUSED(b);
	return -ENODEV;
}

void linkr_status_led_off(void)
{
}

void linkr_status_led_get_rgb(uint8_t *r, uint8_t *g, uint8_t *b)
{
	if (r != NULL) {
		*r = 0;
	}
	if (g != NULL) {
		*g = 0;
	}
	if (b != NULL) {
		*b = 0;
	}
}

void linkr_status_led_blink(int count, int delay_ms)
{
	ARG_UNUSED(count);
	ARG_UNUSED(delay_ms);
}

#endif
