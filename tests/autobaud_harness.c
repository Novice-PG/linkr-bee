/* Host-only fakes around the production autobaud block, sliced in by
 * tests/test_autobaud.py. No Zephyr workqueue, no Espressif silicon: the
 * scenario writes the pulse counters the hardware would have latched and the
 * divider is computed the way src/main.c programs it, so every number the
 * decision sees has been through the real arithmetic. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define ARG_UNUSED(x) (void)(x)
#define K_MSEC(x) (x)
#define LOG_WRN(...) ((void)0)
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))

/*
 * 80 MHz — the source clock the divider arithmetic assumes. The clock
 * cancels out of the production formula, but a simulation still needs one to
 * turn a bit-time into cycles the way the detector would count them.
 */
#define SOURCE_HZ 80000000ull

struct k_work {
	int unused;
};

struct k_work_delayable {
	int unused;
};

typedef struct {
	struct {
		uint32_t glitch_filt;
		uint32_t en;
	} auto_baud;
	struct {
		uint32_t min_cnt;
	} lowpulse;
	struct {
		uint32_t min_cnt;
	} highpulse;
	struct {
		uint32_t div_int;
		uint32_t div_frag;
	} clk_div;
} uart_dev_t;

static uart_dev_t fake_uart;
static uint32_t fake_now_ms;
static uint32_t configured_baud = 115200;
static int reconfigure_calls;
static uint32_t reconfigured_to;

static void k_work_reschedule(struct k_work_delayable *work, int32_t ms)
{
	(void)work;
	(void)ms;
}

static void k_work_init_delayable(struct k_work_delayable *work,
				  void (*handler)(struct k_work *))
{
	(void)work;
	(void)handler;
}

static uint32_t k_uptime_get_32(void)
{
	return fake_now_ms;
}

/* What src/main.c programs: cycles-per-bit x 16, split across the integer
 * and fractional halves of clk_div — 694 + 7/16 at 115200, the numbers the
 * tick's own comment quotes. */
static void set_divider(uint32_t baud)
{
	uint32_t div = (uint32_t)(SOURCE_HZ * 16u / baud);

	fake_uart.clk_div.div_int = div >> 4;
	fake_uart.clk_div.div_frag = div & 0xFu;
}

/* The shortest run the line would latch for `bits` bit-times at `baud`. */
static uint32_t pulse(uint32_t baud, uint32_t bits)
{
	return (uint32_t)(SOURCE_HZ * bits / baud);
}

uint32_t linkr_uart_config_current(void);
int linkr_uart_config_reconfigure(uint32_t baudrate);

#include "autobaud_functions.inc"

uint32_t linkr_uart_config_current(void)
{
	return configured_baud;
}

int linkr_uart_config_reconfigure(uint32_t baudrate)
{
	reconfigure_calls++;
	reconfigured_to = baudrate;
	configured_baud = baudrate;
	set_divider(baudrate);

	return 0;
}

/* Close one window: hand the tick the pulse widths the detector latched and
 * let the real decision read them. */
static void window(uint32_t low, uint32_t high)
{
	fake_now_ms += 400; /* one AUTOBAUD_WINDOW_MS */
	fake_uart.lowpulse.min_cnt = low;
	fake_uart.highpulse.min_cnt = high;
	autobaud_tick(NULL);
}

/* A link that has been measured against the port, followed by a stream with
 * no single-bit run in it anywhere — 0xCC carries runs of two and three bits,
 * both polarities timing the same one, so it reads 57600 in every window
 * while the port sits at 115200. Reaching 57600 would be a guess, and the
 * line never said it. */
static void scenario_verified_then_binary(void)
{
	window(pulse(115200, 1), pulse(115200, 1));

	for (int i = 0; i < 6; i++) {
		window(pulse(115200, 2), pulse(115200, 2));
	}
}

/* Nothing has matched the port yet, so a peer that really is slower is still
 * followed — the reason the feature exists. */
static void scenario_startup_follows_a_slower_peer(void)
{
	window(pulse(9600, 1), pulse(9600, 1));
	window(pulse(9600, 1), pulse(9600, 1));
}

/* A verified link whose peer speeds up: no absent single-bit run can read
 * high, so this direction stays open — it is where the Kconfig's 1500000 /
 * 921600 / 1000000 consoles come from. */
static void scenario_verified_step_up(void)
{
	window(pulse(115200, 1), pulse(115200, 1));
	window(pulse(921600, 1), pulse(921600, 1));
	window(pulse(921600, 1), pulse(921600, 1));
}

/* Verified, then a peer that slowed to a rate our own is *not* divided by:
 * not the shape of a missing single-bit run, so it is still believed. */
static void scenario_verified_other_rate(void)
{
	configured_baud = 1500000;
	set_divider(configured_baud);

	window(pulse(1500000, 1), pulse(1500000, 1));
	window(pulse(115200, 1), pulse(115200, 1));
	window(pulse(115200, 1), pulse(115200, 1));
}

int main(int argc, char **argv)
{
	const char *scenario = argc > 1 ? argv[1] : "";

	/* The production init itself, not a stand-in: it arms the detector,
	 * starts the window, and holds the cooldown and warning clocks. */
	set_divider(configured_baud);
	linkr_uart_autobaud_init((uintptr_t)&fake_uart);

	if (strcmp(scenario, "verified_then_binary") == 0) {
		scenario_verified_then_binary();
	} else if (strcmp(scenario, "startup_follows_a_slower_peer") == 0) {
		scenario_startup_follows_a_slower_peer();
	} else if (strcmp(scenario, "verified_step_up") == 0) {
		scenario_verified_step_up();
	} else if (strcmp(scenario, "verified_other_rate") == 0) {
		scenario_verified_other_rate();
	} else {
		fprintf(stderr, "unknown scenario: %s\n", scenario);
		return 2;
	}

	printf("rate=%u\n", configured_baud);
	printf("reconfigs=%d\n", reconfigure_calls);
	printf("last=%u\n", reconfigured_to);

	return 0;
}
