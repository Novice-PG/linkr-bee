/* Host checks for the auxiliary board I/O command group.
 *
 * src/linkr_aux.c is the text protocol a host uses to drive the Linkr Bee X1.0
 * status LED, optocoupler and IR remote. It has no hardware behaviour of its
 * own, so it is compiled here against stub copies of the three device
 * interfaces (include them via -I src) and exercised as plain strings.
 */
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "linkr_aux.h"
#include "linkr_aiconf.h"
#include "linkr_ir.h"
#include "linkr_opto.h"
#include "linkr_prompt.h"
#include "linkr_status_led.h"

/* ---- Zephyr shims -------------------------------------------------------
 * The production file's '#include <zephyr/...>' lines are stripped by the
 * test runner; everything below stands in for them.
 */
#define snprintk snprintf
#define IS_ENABLED(x) (x)
#define CONFIG_SETTINGS 1
#define CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG 1
#define CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG_MAX 32
#define CONFIG_LINKR_BLE_BRIDGE_IR_LEARN 1
#define CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_TIMEOUT_MS 15000
#define CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX 32
#define LOG_MODULE_DECLARE(...)
#define LOG_WRN(...) ((void)0)
#define LOG_INF(...) ((void)0)

static bool led_present = true;
static bool opto_present = true;
static bool ir_present = true;
static uint8_t led_state_r, led_state_g, led_state_b;
static bool opto_state_closed;
static uint32_t opto_pulse_ms;
static bool ir_rx_on;
static struct linkr_ir_status ir_snapshot;
static unsigned int ir_send_calls;
static uint16_t ir_sent_address;
static uint8_t ir_sent_command;
static uint8_t ir_sent_repeats;
static int32_t ir_send_result;
static unsigned int ir_clear_calls;
static bool learn_running;
static uint8_t learn_slot;
static int32_t learn_stop_result;
static int32_t play_result;
static int32_t forget_result;
static uint16_t slot_edges[LINKR_IR_LEARN_SLOTS];
static char slot_names[LINKR_IR_LEARN_SLOTS]
			[CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX + 1];

bool linkr_status_led_available(void)
{
	return led_present;
}

int linkr_status_led_init(void)
{
	return led_present ? 0 : -ENODEV;
}

void linkr_status_led_activity(void)
{
}

int linkr_status_led_set_rgb(uint8_t r, uint8_t g, uint8_t b)
{
	if (!led_present) {
		return -ENODEV;
	}
	led_state_r = r;
	led_state_g = g;
	led_state_b = b;
	return 0;
}

void linkr_status_led_off(void)
{
	(void)linkr_status_led_set_rgb(0, 0, 0);
}

void linkr_status_led_get_rgb(uint8_t *r, uint8_t *g, uint8_t *b)
{
	*r = led_state_r;
	*g = led_state_g;
	*b = led_state_b;
}

void linkr_status_led_blink(int count, int delay_ms)
{
	(void)count;
	(void)delay_ms;
}

bool linkr_opto_available(void)
{
	return opto_present;
}

int linkr_opto_init(void)
{
	return opto_present ? 0 : -ENODEV;
}

int linkr_opto_set(bool closed)
{
	if (!opto_present) {
		return -ENODEV;
	}
	opto_state_closed = closed;
	return 0;
}

int linkr_opto_pulse(uint32_t duration_ms)
{
	if (!opto_present) {
		return -ENODEV;
	}
	opto_state_closed = true;
	opto_pulse_ms = duration_ms;
	return 0;
}

bool linkr_opto_is_closed(void)
{
	return opto_state_closed;
}

bool linkr_ir_available(void)
{
	return ir_present;
}

int linkr_ir_init(void)
{
	return ir_present ? 0 : -ENODEV;
}

int linkr_ir_send(uint16_t address, uint8_t command, uint8_t repeats)
{
	if (!ir_present) {
		return -ENODEV;
	}
	ir_send_calls++;
	ir_sent_address = address;
	ir_sent_command = command;
	ir_sent_repeats = repeats;
	return (int)ir_send_result;
}

int linkr_ir_set_rx(bool enabled)
{
	if (!ir_present) {
		return -ENODEV;
	}
	ir_rx_on = enabled;
	return 0;
}

void linkr_ir_get_status(struct linkr_ir_status *status)
{
	*status = ir_snapshot;
	status->rx_enabled = ir_rx_on;
	status->learning = learn_running;
	status->learn_slot = learn_slot;
	status->learn_edges = learn_running ? 12 : 0;
	status->slots_used = 0;
	for (uint8_t slot = 0; slot < LINKR_IR_LEARN_SLOTS; slot++) {
		status->slot_edges[slot] = slot_edges[slot];
		if (slot_edges[slot] != 0) {
			status->slots_used++;
		}
	}
}

void linkr_ir_clear(void)
{
	ir_clear_calls++;
}

int linkr_ir_learn_start(uint8_t slot)
{
	if (!ir_present) {
		return -ENODEV;
	}
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}
	learn_running = true;
	learn_slot = slot;
	return 0;
}

int linkr_ir_learn_stop(void)
{
	if (learn_stop_result != 0) {
		return (int)learn_stop_result;
	}
	if (!learn_running) {
		return -EINVAL;
	}
	learn_running = false;
	slot_edges[learn_slot] = 33;
	return 0;
}

int linkr_ir_play(uint8_t slot)
{
	if (!ir_present) {
		return -ENODEV;
	}
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}
	if (play_result != 0) {
		return (int)play_result;
	}
	if (slot_edges[slot] == 0) {
		return -ENOENT;
	}
	return 0;
}

int linkr_ir_forget(uint8_t slot)
{
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}
	if (forget_result != 0) {
		return (int)forget_result;
	}
	if (slot_edges[slot] == 0) {
		return -ENOENT;
	}
	slot_edges[slot] = 0;
	return 0;
}

uint16_t linkr_ir_slot_edges(uint8_t slot)
{
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return 0;
	}
	return slot_edges[slot];
}

int linkr_ir_slot_name_set(uint8_t slot, const char *name)
{
	size_t length;

	if (!ir_present) {
		return -ENODEV;
	}
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}
	length = (name == NULL) ? 0u : strlen(name);
	if (length > CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX) {
		return -EINVAL;
	}
	memset(slot_names[slot], 0, sizeof(slot_names[slot]));
	if (length != 0u) {
		memcpy(slot_names[slot], name, length + 1u);
	}
	return 0;
}

int linkr_ir_slot_name_get(uint8_t slot, char *out, size_t out_len)
{
	size_t length;

	if (!ir_present) {
		return -ENODEV;
	}
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}
	if (out == NULL || out_len == 0u) {
		return -ENOSPC;
	}
	length = strlen(slot_names[slot]);
	if (length + 1u > out_len) {
		return -ENOSPC;
	}
	memcpy(out, slot_names[slot], length + 1u);
	return (int)length;
}

static char prompt_text[LINKR_PROMPT_MAX + 1];
static int prompt_length;

int linkr_prompt_len(void)
{
	return prompt_length;
}

int linkr_prompt_read(uint8_t *out, size_t out_len)
{
	if (prompt_length == 0) {
		return -ENOENT;
	}
	if ((size_t)prompt_length > out_len) {
		return -ENOSPC;
	}
	memcpy(out, prompt_text, (size_t)prompt_length);
	return prompt_length;
}

int linkr_prompt_write(const uint8_t *data, size_t len)
{
	if (len > LINKR_PROMPT_MAX) {
		return -EINVAL;
	}
	memset(prompt_text, 0, sizeof(prompt_text));
	if (len != 0u) {
		memcpy(prompt_text, data, len);
	}
	prompt_length = (int)len;
	return 0;
}

int linkr_prompt_clear(void)
{
	memset(prompt_text, 0, sizeof(prompt_text));
	prompt_length = 0;
	return 0;
}

/* ---- Board-stored AI configuration stubs --------------------------------- */
static uint8_t aiconf_blob[CONFIG_LINKR_BLE_BRIDGE_AI_CONFIG_MAX];
static size_t aiconf_stored;
static size_t aiconf_stage;
static bool aiconf_staging;

int linkr_aiconf_len(void)
{
	return (int)aiconf_stored;
}

int linkr_aiconf_begin(void)
{
	aiconf_stage = 0;
	aiconf_staging = true;
	return 0;
}

int linkr_aiconf_put(const uint8_t *data, size_t len)
{
	if (!aiconf_staging || data == NULL || len == 0u) {
		return -EINVAL;
	}
	if (aiconf_stage + len > sizeof(aiconf_blob)) {
		return -ENOSPC;
	}
	memcpy(aiconf_blob + aiconf_stage, data, len);
	aiconf_stage += len;
	return (int)aiconf_stage;
}

int linkr_aiconf_commit(void)
{
	if (!aiconf_staging) {
		return -EINVAL;
	}
	aiconf_stored = aiconf_stage;
	aiconf_staging = false;
	return 0;
}

int linkr_aiconf_clear(void)
{
	aiconf_stored = 0;
	aiconf_stage = 0;
	aiconf_staging = false;
	return 0;
}

int linkr_aiconf_read(uint16_t offset, uint8_t *out, size_t out_len)
{
	size_t copy;

	if (aiconf_staging) {
		return -EBUSY;
	}
	if (aiconf_stored == 0u) {
		return -ENOENT;
	}
	if (offset >= aiconf_stored) {
		return 0;
	}
	copy = aiconf_stored - offset;
	if (copy > out_len) {
		copy = out_len;
	}
	memcpy(out, aiconf_blob + offset, copy);
	return (int)copy;
}

/* ---- Production code, with only its Zephyr includes removed ------------- */
#include "aux_production.inc"
#include "control_limit.inc"

/* ---- Checks ------------------------------------------------------------- */

static const char *aux_help_text(void)
{
	static char buffer[LINKR_AUX_HELP_MAX];

	linkr_aux_help(buffer, sizeof(buffer));
	return buffer;
}

static const char *run(const char *command, char *response, size_t size)
{
	bool handled = linkr_aux_command(command, response, size);

	if (!handled) {
		fprintf(stderr, "command '%s' was not handled\n", command);
		assert(0);
	}
	return response;
}

static void expect_starts(const char *command, const char *prefix)
{
	char response[192];

	run(command, response, sizeof(response));
	if (strncmp(response, prefix, strlen(prefix)) != 0) {
		fprintf(stderr, "command '%s' answered '%s', want '%s...'\n",
			command, response, prefix);
		assert(0);
	}
}

static void test_led_commands(void)
{
	char response[192];
	const uint8_t rgb[3] = {1, 2, 3};

	expect_starts("led=1,2,3", "OK led rgb=010203\r\n");
	assert(led_state_r == rgb[0] && led_state_g == rgb[1] && led_state_b == rgb[2]);

	expect_starts("l=0x10,0x20,0x30", "OK led rgb=102030\r\n");
	expect_starts("l= 7 , 8 , 9", "OK led rgb=070809\r\n");
	expect_starts("led?", "OK led rgb=070809\r\n");
	expect_starts("l off", "OK led off\r\n");
	expect_starts("l?", "OK led rgb=000000\r\n");

	expect_starts("led=1,2", "ERR led rgb=");
	expect_starts("led=1,2,3 ", "OK led rgb=010203\r\n");
	expect_starts("led=1,2,300", "ERR led rgb=");
	expect_starts("led=1,2,3,4", "ERR led rgb=");

	/* Feature absent: the command still answers, with the reason. */
	led_present = false;
	run("l?", response, sizeof(response));
	expect_starts("l=1,2,3", "ERR led unsupported\r\n");
	expect_starts("l off", "ERR led unsupported\r\n");
	led_present = true;
}

static void test_opto_commands(void)
{
	expect_starts("opto on", "OK opto=closed\r\n");
	assert(opto_state_closed);
	expect_starts("o?", "OK opto=closed\r\n");

	expect_starts("opto off", "OK opto=open\r\n");
	assert(!opto_state_closed);
	expect_starts("o?", "OK opto=open\r\n");

	expect_starts("o pulse 250", "OK opto=pulse ms=250\r\n");
	assert(opto_state_closed && opto_pulse_ms == 250);

	expect_starts("o pulse 0", "ERR opto pulse=");
	expect_starts("o pulse 60001", "ERR opto pulse=");
	expect_starts("o pulse", "ERR opto pulse=");
	expect_starts("o sideways", "ERR opto=");

	opto_present = false;
	expect_starts("o on", "ERR opto unsupported\r\n");
	opto_present = true;
}

static void test_ir_commands(void)
{
	expect_starts("ir tx 0x1234,0x56,3", "OK ir tx addr=0x1234,cmd=0x56,repeats=3\r\n");
	assert(ir_send_calls == 1);
	assert(ir_sent_address == 0x1234 && ir_sent_command == 0x56 && ir_sent_repeats == 3);

	expect_starts("r tx 4,8", "OK ir tx addr=0x0004,cmd=0x08,repeats=0\r\n");
	expect_starts("ir send 4,8,1", "OK ir tx addr=0x0004,cmd=0x08,repeats=1\r\n");

	expect_starts("ir tx 1,2,99", "ERR ir repeats=");
	expect_starts("ir tx 0x10000,2", "ERR ir tx=");
	expect_starts("ir tx 1", "ERR ir tx=");
	expect_starts("ir tx", "ERR ir tx=");

	expect_starts("ir rx on", "OK ir rx=on\r\n");
	assert(ir_rx_on);
	expect_starts("ir rx off", "OK ir rx=off\r\n");
	assert(!ir_rx_on);
	expect_starts("ir rx maybe", "ERR ir rx=");

	/* A received frame is reported with address, command and the extended flag. */
	memset(&ir_snapshot, 0, sizeof(ir_snapshot));
	ir_snapshot.have_frame = true;
	ir_snapshot.frames = 2;
	ir_snapshot.repeats = 1;
	ir_snapshot.last.address = 0x1234;
	ir_snapshot.last.command = 0x56;
	ir_snapshot.last.extended = true;
	expect_starts("r?", "OK ir rx=off frames=2 repeats=1 glitches=0 "
			    "last=addr=0x1234,cmd=0x56,ext=yes learn=idle,slots=0/8\r\n");

	memset(&ir_snapshot, 0, sizeof(ir_snapshot));
	expect_starts("ir?", "OK ir rx=off frames=0 repeats=0 glitches=0 last=none "
			     "learn=idle,slots=0/8\r\n");

	expect_starts("r clear", "OK ir cleared\r\n");
	assert(ir_clear_calls == 1);

	/* A full transmit queue is reported as busy, not as absent hardware. */
	ir_send_result = -ENOMSG;
	expect_starts("r tx 1,2", "ERR ir busy\r\n");
	ir_send_result = 0;

	ir_present = false;
	expect_starts("r tx 1,2", "ERR ir unsupported\r\n");
	expect_starts("r rx on", "ERR ir unsupported\r\n");
	ir_present = true;
}

static void test_ir_learning_commands(void)
{
	memset(slot_edges, 0, sizeof(slot_edges));
	learn_running = false;
	learn_stop_result = 0;
	play_result = 0;
	forget_result = 0;

	expect_starts("ir learn", "OK ir learn slot=0 timeout_ms=15000\r\n");
	assert(learn_running && learn_slot == 0);

	expect_starts("r learn 3", "OK ir learn slot=3 timeout_ms=15000\r\n");
	assert(learn_running && learn_slot == 3);

	expect_starts("r learn 8", "ERR ir slot=0-7\r\n");
	assert(learn_slot == 3);

	expect_starts("ir learn stop", "OK ir learn slot=3 stored edges=33\r\n");
	assert(!learn_running && slot_edges[3] == 33);
	assert(linkr_ir_slot_edges(3) == 33);

	expect_starts("ir learn stop", "ERR ir learn=idle\r\n");

	expect_starts("r play", "ERR ir slot=0 empty\r\n");
	expect_starts("r play 3", "OK ir play slot=3 edges=33\r\n");

	expect_starts("r slots", "OK ir slots=0:0,1:0,2:0,3:33,4:0,5:0,6:0,7:0 used=1\r\n");

	expect_starts("r delete 3", "OK ir slot=3 cleared\r\n");
	expect_starts("r delete 3", "ERR ir slot=3 empty\r\n");
	expect_starts("r delete 9", "ERR ir slot=0-7\r\n");
	expect_starts("r forget 0", "ERR ir slot=0 empty\r\n");

	memset(&ir_snapshot, 0, sizeof(ir_snapshot));
	expect_starts("r?", "OK ir rx=off frames=0 repeats=0 glitches=0 last=none "
			    "learn=idle,slots=0/8\r\n");

	/* A capture that never produced a frame reports no-frame, not a slot. */
	learn_stop_result = -EAGAIN;
	learn_running = true;
	expect_starts("r learn stop", "ERR ir learn=no-frame\r\n");
	learn_running = false;
	learn_stop_result = 0;

	expect_starts("r play 0", "ERR ir slot=0 empty\r\n");
	play_result = -ENOMSG;
	slot_edges[0] = 10;
	expect_starts("r play 0", "ERR ir busy\r\n");
	play_result = 0;

	ir_present = false;
	expect_starts("r learn", "ERR ir unsupported\r\n");
	expect_starts("r play 0", "ERR ir unsupported\r\n");
	ir_present = true;
	learn_stop_result = 0;
}

static void test_ir_slot_names(void)
{
	memset(slot_names, 0, sizeof(slot_names));
	ir_present = true;
	learn_running = false;

	expect_starts("r name 0", "OK ir name=0 none\r\n");

	/* "客厅" in UTF-8, moved as hex so spaces/Chinese need no quoting. */
	expect_starts("r name 0 e5aea2e58e85", "OK ir name=0 set\r\n");
	expect_starts("r name 0", "OK ir name=0 hex=e5aea2e58e85\r\n");
	assert(strcmp(slot_names[0], "客厅") == 0);

	/* ASCII names with spaces survive the round trip too. */
	expect_starts("ir name 1 4c6976696e6720726f6f6d", "OK ir name=1 set\r\n");
	expect_starts("r name 1", "OK ir name=1 hex=4c6976696e6720726f6f6d\r\n");
	assert(strcmp(slot_names[1], "Living room") == 0);

	expect_starts("r name 0 clear", "OK ir name=0 cleared\r\n");
	expect_starts("r name 0", "OK ir name=0 none\r\n");
	expect_starts("r name 1 clear", "OK ir name=1 cleared\r\n");

	expect_starts("r name 0 zz", "ERR ir name=hex\r\n");
	expect_starts("r name 0 abc", "ERR ir name=hex\r\n");
	expect_starts("r name 0 00", "ERR ir name=hex\r\n");
	expect_starts("r name 8", "ERR ir slot=0-7\r\n");

	/* 33 bytes exceed the 32-byte cap. */
	expect_starts("r name 0 "
		      "616161616161616161616161616161616161616161616161616161616161616161",
		      "ERR ir name=too-long\r\n");

	ir_present = false;
	expect_starts("r name 0", "ERR ir unsupported\r\n");
	expect_starts("r name 0 4142", "ERR ir unsupported\r\n");
	expect_starts("r name 0 clear", "ERR ir unsupported\r\n");
	ir_present = true;
}

static void test_prompt_commands(void)
{
	char command[600];
	char hex[2 * 100 + 1];

	prompt_length = 0;
	prompt_text[0] = '\0';

	expect_starts("p?", "OK prompt=0\r\n");

	/* "hello" */
	expect_starts("p set 68656c6c6f", "OK prompt len=5\r\n");
	expect_starts("p?", "OK prompt=5\r\n");
	expect_starts("p get", "OK p off=0 hex=68656c6c6f total=5 done=yes\r\n");
	expect_starts("prompt get 2", "OK p off=2 hex=6c6c6f total=5 done=yes\r\n");
	expect_starts("p get 99", "OK p off=5 hex= total=5 done=yes\r\n");

	expect_starts("p clear", "OK prompt cleared\r\n");
	expect_starts("p get", "ERR prompt=empty\r\n");
	expect_starts("p clear", "OK prompt cleared\r\n");

	/* Long prompts are served in 48-byte hex chunks. */
	for (int i = 0; i < 100; i++) {
		snprintf(&hex[i * 2], 3, "61"); /* 'a' */
	}
	snprintf(command, sizeof(command), "p set %s", hex);
	expect_starts(command, "OK prompt len=100\r\n");
	expect_starts("p get", "OK p off=0 hex=");
	expect_starts("p get 96", "OK p off=96 hex=61616161 total=100 done=yes\r\n");

	expect_starts("p set zz", "ERR prompt=hex\r\n");
	expect_starts("p set 00", "ERR prompt=hex\r\n");
	expect_starts("p bogus", "ERR prompt=expected set|get|clear\r\n");

	/* A full prompt passes the actual main.c command-length gate. */
	char max_hex[2 * LINKR_PROMPT_MAX + 1];
	for (int i = 0; i < LINKR_PROMPT_MAX; i++) {
		snprintf(&max_hex[i * 2], 3, "61");
	}
	snprintf(command, sizeof(command), "@p set %s", max_hex);
	assert(!command_rejected((uint16_t)strlen(command)));
	expect_starts(command + 1, "OK prompt len=240\r\n");
	assert(!command_rejected(LINKR_MGMT_MAX_PAYLOAD));
	assert(command_rejected(LINKR_MGMT_MAX_PAYLOAD + 1));

	/* More than 240 bytes no longer fits the hex decoder. */
	char long_hex[2 * 241 + 1];
	for (int i = 0; i < 241; i++) {
		snprintf(&long_hex[i * 2], 3, "61");
	}
	snprintf(command, sizeof(command), "p set %s", long_hex);
	expect_starts(command, "ERR prompt=hex\r\n");
}

static void test_aiconf_commands(void)
{
	char chunk_hex[2 * 20 + 1];
	char command[64];

	for (int i = 0; i < 20; i++) {
		snprintf(&chunk_hex[i * 2], 3, "61");
	}
	snprintf(command, sizeof(command), "y put %s", chunk_hex);

	aiconf_stored = 0;
	aiconf_stage = 0;
	aiconf_staging = false;

	expect_starts("y?", "OK aiconf bytes=0 max=32\r\n");
	expect_starts("y put 61", "ERR aiconf=not-begun\r\n");
	expect_starts("y get", "ERR aiconf=empty\r\n");

	expect_starts("y begin", "OK aiconf begin\r\n");
	expect_starts("y put 616263", "OK aiconf stage=3\r\n");
	expect_starts("aiconf put 6465", "OK aiconf stage=5\r\n");
	expect_starts("y get", "ERR aiconf=empty\r\n");
	expect_starts("y end", "OK aiconf bytes=5\r\n");
	expect_starts("y?", "OK aiconf bytes=5 max=32\r\n");
	expect_starts("y get", "OK aiconf off=0 hex=6162636465 total=5 done=yes\r\n");
	expect_starts("y get 3", "OK aiconf off=3 hex=6465 total=5 done=yes\r\n");
	expect_starts("y get 9", "OK aiconf off=9 hex= total=5 done=yes\r\n");

	expect_starts("y clear", "OK aiconf cleared\r\n");
	expect_starts("y?", "OK aiconf bytes=0 max=32\r\n");

	/* The staging budget is the configured maximum (32 bytes here). */
	expect_starts("y begin", "OK aiconf begin\r\n");
	expect_starts(command, "OK aiconf stage=20\r\n");
	expect_starts(command, "ERR aiconf=too-large\r\n");

	expect_starts("y put zz", "ERR aiconf=hex\r\n");
	expect_starts("y bogus", "ERR aiconf=expected begin|put|end|get|clear\r\n");
}

static void test_dispatch_and_help(void)
{
	char response[192];

	/* Commands outside the group are left to the other handlers. */
	assert(!linkr_aux_command("w?", response, sizeof(response)));
	assert(!linkr_aux_command("help", response, sizeof(response)));
	assert(!linkr_aux_command("lorem", response, sizeof(response)));

	assert(linkr_aux_init() == 0);
	assert(strstr(aux_help_text(), "@l?") != NULL);
	assert(strstr(aux_help_text(), "@o pulse <ms>") != NULL);
	assert(strstr(aux_help_text(), "@r tx") != NULL);
	assert(strstr(aux_help_text(), "@r learn") != NULL);
	assert(strstr(aux_help_text(), "@r name") != NULL);
	assert(strstr(aux_help_text(), "@p set") != NULL);
	assert(strstr(aux_help_text(), "@y begin") != NULL);

	/* Absent hardware is a normal board variant, not an init failure: the
	 * helper reports only real errors and the help list drops those groups.
	 * Prompts live in NVS, so they stay available without the three devices. */
	led_present = false;
	opto_present = false;
	ir_present = false;
	assert(linkr_aux_init() == 0);
	assert(strstr(aux_help_text(), "@l?") == NULL);
	assert(strstr(aux_help_text(), "@r tx") == NULL);
	assert(strstr(aux_help_text(), "@p set") != NULL);
	assert(strstr(aux_help_text(), "@y begin") != NULL);
	led_present = true;
	opto_present = true;
	ir_present = true;
}

int main(void)
{
	assert(linkr_aux_is_readonly("p get"));
	assert(linkr_aux_is_readonly("prompt get 48"));
	assert(linkr_aux_is_readonly("y get 0"));
	assert(!linkr_aux_is_readonly("p getclear"));
	assert(!linkr_aux_is_readonly("p get 0 clear"));
	assert(!linkr_aux_is_readonly("o on"));
	assert(!linkr_aux_is_readonly("r tx 1,2"));
	assert(!linkr_aux_is_readonly("y begin"));

	test_led_commands();
	test_opto_commands();
	test_ir_commands();
	test_ir_learning_commands();
	test_ir_slot_names();
	test_prompt_commands();
	test_aiconf_commands();
	test_dispatch_and_help();
	printf("aux command harness: ok\n");
	return 0;
}
