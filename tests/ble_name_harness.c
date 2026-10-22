/* Host-only harness around the production BLE-name logic in src/main.c.
 *
 * The two functions under test — the name check and the name install — are
 * sliced out of the firmware by tests/test_ble_name.py and compiled here
 * against fakes for the Bluetooth stack. What is exercised is the firmware's
 * own code: the byte and character rules, and the two pieces of state that
 * have to move together — the name buffer and the length the scan response
 * advertises with it.
 *
 * SPDX-License-Identifier: Apache-2.0 */

#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define ARG_UNUSED(x) (void)(x)

/* The firmware's own limit, mirrored so the harness and the firmware cannot
 * drift apart without the test noticing. */
#define BLE_NAME_MAX 29

/* Zephyr's advertising data entry, reduced to the three fields the name
 * touches. */
struct bt_data {
	uint8_t type;
	uint8_t data_len;
	uint8_t *data;
};

static char ble_name[BLE_NAME_MAX + 1];
static struct bt_data sd[] = {
	{ 0, 0, (uint8_t *)ble_name },
};

/* The stack refuses the name for one scenario: a board whose controller will
 * not take the name must not end up advertising half of it. */
static bool bt_set_name_fails;

static int bt_set_name(const char *name)
{
	if (bt_set_name_fails) {
		return -EINVAL;
	}
	ARG_UNUSED(name);
	return 0;
}

#include "ble_name_production.inc"

static void print_name(const char *label, const char *name)
{
	printf("%s=%s\n", label, name);
}

static void print_state(const char *label)
{
	printf("%s_buffer=%s\n", label, ble_name);
	printf("%s_length=%u\n", label, (unsigned int)sd[0].data_len);
}

int main(int argc, char **argv)
{
	const char *scenario = argc > 1 ? argv[1] : "validate";

	if (!strcmp(scenario, "validate")) {
		/* The rules the firmware applies to a name, as the firmware
		 * applies them. */
		printf("plain=%d\n", ble_name_valid("Bench Bee", 9) ? 1 : 0);
		printf("chinese=%d\n", ble_name_valid("测试", 6) ? 1 : 0);
		printf("emoji7=%d\n", ble_name_valid("😀😀😀😀😀😀😀", 28) ? 1 : 0);
		printf("emoji8=%d\n", ble_name_valid("😀😀😀😀😀😀😀😀", 32) ? 1 : 0);
		printf("empty=%d\n", ble_name_valid("", 0) ? 1 : 0);
		printf("control=%d\n", ble_name_valid("one\ntwo", 7) ? 1 : 0);
		printf("del=%d\n", ble_name_valid("a\x7f" "b", 3) ? 1 : 0);
		printf("c1=%d\n", ble_name_valid("a\xc2\x85" "b", 4) ? 1 : 0);
		printf("overlong=%d\n", ble_name_valid("\xc0\x80", 2) ? 1 : 0);
		printf("truncated=%d\n", ble_name_valid("\xe6\xb5", 2) ? 1 : 0);
		printf("surrogate=%d\n", ble_name_valid("\xed\xa0\x80", 3) ? 1 : 0);
		return 0;
	}

	if (!strcmp(scenario, "apply")) {
		int err = ble_name_apply("测试😀");

		printf("err=%d\n", err);
		print_state("applied");
		print_name("name", ble_name);
		return 0;
	}

	if (!strcmp(scenario, "apply_fail")) {
		int err;

		bt_set_name_fails = true;
		memcpy(ble_name, "Linkr Bee 078C", 15);
		sd[0].data_len = 15;
		err = ble_name_apply("测试😀");
		printf("err=%d\n", err);
		print_state("rejected");
		return 0;
	}

	if (!strcmp(scenario, "apply_same_length")) {
		int err = ble_name_apply("Bench Bee");

		printf("err=%d\n", err);
		print_state("applied");
		return 0;
	}

	fprintf(stderr, "unknown scenario: %s\n", scenario);
	return 2;
}
