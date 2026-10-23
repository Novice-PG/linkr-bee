/* SPDX-License-Identifier: Apache-2.0 */
#include "watchdog.h"

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/hwinfo.h>
#include <zephyr/fatal.h>
#include <zephyr/init.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/logging/log_ctrl.h>
#include <zephyr/sys/reboot.h>
#include <zephyr/task_wdt/task_wdt.h>

LOG_MODULE_REGISTER(linkr_watchdog, LOG_LEVEL_INF);

BUILD_ASSERT(DT_NODE_HAS_STATUS(DT_ALIAS(watchdog0), okay),
	     "Linkr watchdog requires the board's watchdog0 device");
BUILD_ASSERT(CONFIG_TASK_WDT_CHANNELS >= LINKR_WDT_CHANNEL_COUNT);
BUILD_ASSERT(!IS_ENABLED(CONFIG_TASK_WDT_DUMMY),
	     "Production watchdog must not use the dummy implementation");

#define FEED_INTERVAL_MS 1000

static const char *const channel_names[] = {
	"startup", "system workqueue", "UART RX", "UART TX",
	"management request", "management TX",
};
static int channels[LINKR_WDT_CHANNEL_COUNT];
/* Written only by the owning thread/work item, after initialization. */
static int64_t last_feed[LINKR_WDT_CHANNEL_COUNT];
static struct k_work_delayable health_work;

static void watchdog_expired(int channel_id, void *user_data)
{
	ARG_UNUSED(channel_id);
	/* ISR context: do not acquire application locks or flush deferred logs. */
	printk("Linkr watchdog timeout: %s; rebooting\n", (const char *)user_data);
	sys_reboot(SYS_REBOOT_COLD);
}

void linkr_watchdog_feed(enum linkr_watchdog_channel channel)
{
	int64_t now = k_uptime_get();

	/* UART RX can visit here on every packet. Avoid a kernel-timer restart
	 * for every byte/chunk while still requiring progress from each owner. */
	if (now - last_feed[channel] >= FEED_INTERVAL_MS) {
		(void)task_wdt_feed(channels[channel]);
		last_feed[channel] = now;
	}
}

static void health_check(struct k_work *work)
{
	ARG_UNUSED(work);
	linkr_watchdog_feed(LINKR_WDT_SYSWORK);
	k_work_reschedule(&health_work, K_MSEC(FEED_INTERVAL_MS));
}

void linkr_watchdog_boot_complete(void)
{
	/* main returns after startup; the other channels remain supervised. */
	(void)task_wdt_delete(channels[LINKR_WDT_STARTUP]);
}

static int watchdog_init(void)
{
	const struct device *hardware = DEVICE_DT_GET(DT_ALIAS(watchdog0));
	uint32_t reset_cause;
	int err;

	if (!device_is_ready(hardware)) {
		LOG_ERR("Hardware watchdog unavailable");
		k_panic();
	}
	err = task_wdt_init(hardware);
	if (err) {
		LOG_ERR("Watchdog initialization failed: %d", err);
		k_panic();
	}
	for (int i = 0; i < LINKR_WDT_CHANNEL_COUNT; i++) {
		channels[i] = task_wdt_add(CONFIG_LINKR_BLE_BRIDGE_WATCHDOG_TIMEOUT_MS,
					 watchdog_expired, (void *)channel_names[i]);
		if (channels[i] < 0) {
			LOG_ERR("Watchdog channel %s failed: %d",
				channel_names[i], channels[i]);
			k_panic();
		}
		last_feed[i] = k_uptime_get();
	}
	if (hwinfo_get_reset_cause(&reset_cause) == 0) {
		LOG_INF("Reset cause: 0x%08x", reset_cause);
		(void)hwinfo_clear_reset_cause();
	}
	k_work_init_delayable(&health_work, health_check);
	k_work_reschedule(&health_work, K_MSEC(FEED_INTERVAL_MS));
	LOG_INF("Watchdog active: task timeout %u ms, hardware fallback enabled",
		CONFIG_LINKR_BLE_BRIDGE_WATCHDOG_TIMEOUT_MS);
	return 0;
}

/* Run before static application threads start, including management workers.
 * The startup channel also detects a main() error return or initialization hang. */
SYS_INIT(watchdog_init, APPLICATION, CONFIG_APPLICATION_INIT_PRIORITY);

void k_sys_fatal_error_handler(unsigned int reason, const struct arch_esf *esf)
{
	ARG_UNUSED(esf);
	LOG_ERR("Fatal error %u; rebooting", reason);
	LOG_PANIC();
	/* If logging itself is wedged, the already-armed hardware WDT still resets. */
	sys_reboot(SYS_REBOOT_COLD);
}
