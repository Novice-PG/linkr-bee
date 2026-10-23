/* Host fakes exercise the production watchdog policy, not ESP32 hardware. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <setjmp.h>
#include <stdio.h>
#include <string.h>

#define CONFIG_LINKR_BLE_BRIDGE_WATCHDOG 1
#define CONFIG_LINKR_BLE_BRIDGE_WATCHDOG_TIMEOUT_MS 60000
#define CONFIG_TASK_WDT_CHANNELS 6
#define CONFIG_TASK_WDT_DUMMY 0
#define IS_ENABLED(x) (x)
#define BUILD_ASSERT(condition, ...) _Static_assert(condition, #condition)
#define DT_NODE_HAS_STATUS(...) 1
#define DEVICE_DT_GET(...) (&fake_hardware)
#define ARG_UNUSED(x) (void)(x)
#define K_MSEC(x) (x)
#define SYS_REBOOT_COLD 0
#define LOG_MODULE_REGISTER(...)
#define LOG_ERR(...) ((void)snprintf(message, sizeof(message), __VA_ARGS__))
#define LOG_INF(...) ((void)0)
#define LOG_PANIC() (++panic_flushes)
#define SYS_INIT(...)
#define printk(...) snprintf(message, sizeof(message), __VA_ARGS__)

struct device { int unused; };
struct arch_esf { int unused; };
struct k_work { int unused; };
struct k_work_delayable { void (*handler)(struct k_work *); };
static struct device fake_hardware;
static bool ready = true;
static int64_t now;
static int init_error, add_error_at = -1, allocated, deleted = -1;
static int reboots, panic_flushes, feed_count[6], scheduled;
static char message[128];
static jmp_buf reset;
static void (*callbacks[6])(int, void *);
static void *callback_data[6];
static uint32_t periods[6];
static bool device_is_ready(const struct device *dev) { assert(dev == &fake_hardware); return ready; }
static int64_t k_uptime_get(void) { return now; }
static void sys_reboot(int type) { assert(type == SYS_REBOOT_COLD); reboots++; longjmp(reset, 1); }
void k_sys_fatal_error_handler(unsigned int reason, const struct arch_esf *esf);
static void k_panic(void) { k_sys_fatal_error_handler(4, NULL); }
static int task_wdt_init(const struct device *dev) { assert(dev == &fake_hardware); return init_error; }
static int task_wdt_add(uint32_t period, void (*callback)(int, void *), void *data)
{
	if (allocated == add_error_at) return -12;
	int id = allocated++;
	assert(id < 6);
	periods[id] = period;
	callbacks[id] = callback;
	callback_data[id] = data;
	return id;
}
static int task_wdt_feed(int id) { assert(id >= 0 && id < allocated); feed_count[id]++; return 0; }
static int task_wdt_delete(int id) { deleted = id; return 0; }
static int hwinfo_get_reset_cause(uint32_t *cause) { *cause = 0x20; return 0; }
static int hwinfo_clear_reset_cause(void) { return 0; }
static void k_work_init_delayable(struct k_work_delayable *work, void (*handler)(struct k_work *)) { work->handler = handler; }
static int k_work_reschedule(struct k_work_delayable *work, int delay) { assert(work->handler); scheduled = delay; return 0; }

#include "watchdog_production.inc"

int main(int argc, char **argv)
{
	assert(argc == 2);
	const char *scenario = argv[1];
	if (strcmp(scenario, "device-failure") == 0) ready = false;
	if (strcmp(scenario, "init-failure") == 0) init_error = -5;
	if (strcmp(scenario, "channel-failure") == 0) add_error_at = 2;
	if (setjmp(reset) == 0) {
		assert(watchdog_init() == 0);
		assert(allocated == LINKR_WDT_CHANNEL_COUNT);
		for (int i = 0; i < allocated; i++) assert(periods[i] == 60000);
		assert(scheduled == 1000);
		if (strcmp(scenario, "feeds") == 0) {
			for (now = 0; now < 1000; now++) linkr_watchdog_feed(LINKR_WDT_UART_RX);
			assert(feed_count[LINKR_WDT_UART_RX] == 0);
			linkr_watchdog_feed(LINKR_WDT_UART_RX);
			assert(feed_count[LINKR_WDT_UART_RX] == 1);
			for (int i = 0; i < allocated; i++) {
				if (i != LINKR_WDT_UART_RX) assert(feed_count[i] == 0);
			}
			health_work.handler(NULL);
			assert(feed_count[LINKR_WDT_SYSWORK] == 1);
			assert(feed_count[LINKR_WDT_MGMT_REQUEST] == 0);
			linkr_watchdog_boot_complete();
			assert(deleted == LINKR_WDT_STARTUP);
		} else if (strcmp(scenario, "fatal") == 0) {
			k_sys_fatal_error_handler(2, NULL);
			assert(false);
		} else if (strcmp(scenario, "expired") == 0) {
			callbacks[LINKR_WDT_UART_TX](LINKR_WDT_UART_TX, callback_data[LINKR_WDT_UART_TX]);
			assert(false);
		}
	}
	if (strcmp(scenario, "feeds") == 0) assert(reboots == 0);
	else assert(reboots == 1);
	if (strcmp(scenario, "expired") == 0) assert(strstr(message, "UART TX"));
	if (strcmp(scenario, "fatal") == 0) assert(panic_flushes == 1);
	puts("ok");
	return 0;
}
