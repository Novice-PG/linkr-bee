/*
 * Linkr Bee X1.0 optocoupler switch: GPIO10 low closes the J5 contact.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_opto.h"

#include <errno.h>

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/gpio.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/sys/util.h>

/* Shares main.c's log source: same prefix, no second source slot in RAM. */
LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

/* Longest pulse the command accepts, so a bad request cannot hold the line. */
#define LINKR_OPTO_MAX_PULSE_MS 60000u

#if DT_HAS_ALIAS(linkr_opto)

static const struct gpio_dt_spec opto_gpio =
	GPIO_DT_SPEC_GET(DT_ALIAS(linkr_opto), gpios);
static struct k_work_delayable opto_release_work;
static bool opto_ready;
static bool opto_closed;

static void opto_release(struct k_work *work)
{
	ARG_UNUSED(work);
	(void)linkr_opto_set(false);
}

bool linkr_opto_available(void)
{
	return true;
}

int linkr_opto_init(void)
{
	int err;

	if (!device_is_ready(opto_gpio.port)) {
		return -ENODEV;
	}

	/* GPIO_ACTIVE_LOW in the binding: logical 0 (open) drives the pin high. */
	err = gpio_pin_configure_dt(&opto_gpio, GPIO_OUTPUT_INACTIVE);
	if (err) {
		return err;
	}

	k_work_init_delayable(&opto_release_work, opto_release);
	opto_ready = true;
	opto_closed = false;
	return 0;
}

int linkr_opto_set(bool closed)
{
	int err;

	if (!opto_ready) {
		return -ENODEV;
	}

	if (!closed) {
		/* Cancel first: releasing must not be undone by a late timer. */
		(void)k_work_cancel_delayable(&opto_release_work);
	}

	err = gpio_pin_set_dt(&opto_gpio, closed ? 1 : 0);
	if (err) {
		return err;
	}

	if (opto_closed != closed) {
		LOG_INF("Opto contact %s", closed ? "closed" : "open");
	}
	opto_closed = closed;
	return 0;
}

int linkr_opto_pulse(uint32_t duration_ms)
{
	int err;

	if (duration_ms == 0 || duration_ms > LINKR_OPTO_MAX_PULSE_MS) {
		return -EINVAL;
	}

	err = linkr_opto_set(true);
	if (err) {
		return err;
	}

	(void)k_work_reschedule(&opto_release_work, K_MSEC(duration_ms));
	return 0;
}

bool linkr_opto_is_closed(void)
{
	return opto_closed;
}

#else /* !DT_HAS_ALIAS(linkr_opto) */

bool linkr_opto_available(void)
{
	return false;
}

int linkr_opto_init(void)
{
	return -ENODEV;
}

int linkr_opto_set(bool closed)
{
	ARG_UNUSED(closed);
	return -ENODEV;
}

int linkr_opto_pulse(uint32_t duration_ms)
{
	ARG_UNUSED(duration_ms);
	return -ENODEV;
}

bool linkr_opto_is_closed(void)
{
	return false;
}

#endif /* DT_HAS_ALIAS(linkr_opto) */
