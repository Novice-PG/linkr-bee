/*
 * Linkr Bee X1.0 infrared transceiver: NEC codec, raw learning, LEDC carrier.
 *
 * SPDX-License-Identifier: Apache-2.0
 */
#include "linkr_ir.h"

#include <errno.h>
#include <string.h>

#include <zephyr/device.h>
#include <zephyr/devicetree.h>
#include <zephyr/drivers/gpio.h>
#include <zephyr/drivers/pwm.h>
#include <zephyr/kernel.h>
#include <zephyr/logging/log.h>
#include <zephyr/settings/settings.h>
#include <zephyr/sys/printk.h>
#include <zephyr/sys/util.h>

/* Shares main.c's log source: same prefix, no second source slot in RAM. */
LOG_MODULE_DECLARE(linkr_ble_bridge, LOG_LEVEL_INF);

#if DT_HAS_ALIAS(linkr_ir)

#define LINKR_IR_NODE DT_ALIAS(linkr_ir)

/*
 * The receiver output carries the demodulated envelope, so a gap this long
 * means the previous frame is over and the NEC decoder must resynchronise.
 */
#define LINKR_IR_RX_IDLE_RESET_US 30000u

/* One NEC data frame is 68 ms of busy-waiting. A cooperative helper thread
 * makes that deterministic: only interrupts can stretch a mark, whereas a
 * preemptible thread would let BLE/WiFi unwind the carrier mid-frame. */
#define LINKR_IR_TX_THREAD_PRIORITY K_PRIO_COOP(3)
#define LINKR_IR_TX_THREAD_STACK 1280

/* Repeat frames are sent 40 ms apart, like a held remote key. */
#define LINKR_IR_REPEAT_GAP_MS 40u
#define LINKR_IR_MAX_QUEUED 4

/* Learned frames contain gaps (an RC5 frame is mostly silence); sleeping them
 * keeps the cooperative thread from blocking the rest of the system. */
#define LINKR_IR_LONG_SPACE_US 5000u

#define LINKR_IR_LEARN_POLL_MS 20u

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
_Static_assert(LINKR_IR_LEARN_SLOTS >= 1,
	       "IR learning needs at least one slot");
_Static_assert(LINKR_IR_LEARN_MAX_EDGES <= 65535,
	       "learned durations are stored as uint16 microseconds");
#endif

static const struct gpio_dt_spec ir_rx =
	GPIO_DT_SPEC_GET(LINKR_IR_NODE, rx_gpios);

static struct gpio_callback ir_rx_cb;
static struct linkr_nec_decoder ir_decoder;
static uint32_t ir_last_cycles;
static bool ir_last_valid;
static bool ir_ready;
static bool ir_rx_enabled;
static struct linkr_ir_status ir_counters;
static struct k_spinlock ir_lock;

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
static struct linkr_ir_learn learn_stage;
/*
 * Saved frames live in NVS (flash). RAM keeps only their edge count: enough
 * for @r slots, @r? and the empty/busy check, while a replay reads the one
 * waveform it is about to send. This replaced an 8 * ~390 B RAM cache.
 */
static uint16_t ir_slot_edge_counts[LINKR_IR_LEARN_SLOTS];
static bool ir_learning;
static bool ir_learn_started;
static bool ir_learn_commit_pending;
static uint8_t ir_learn_slot;
static uint32_t ir_learn_start_cycles;
static uint32_t ir_learn_last_cycles;
static enum linkr_ir_learn_step ir_learn_result;
static struct k_work_delayable ir_learn_poll_work;
#endif

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_TX) && DT_NODE_HAS_PROP(LINKR_IR_NODE, pwms)

static const struct pwm_dt_spec ir_pwm = PWM_DT_SPEC_GET(LINKR_IR_NODE);
static bool ir_tx_ready;
K_SEM_DEFINE(ir_tx_init_sem, 0, 1);

enum ir_request_kind {
	IR_REQUEST_NEC = 0,
	IR_REQUEST_RAW,
};

struct ir_request {
	uint8_t kind;
	uint8_t slot;
	uint8_t repeats;
	uint16_t address;
	uint8_t command;
};

K_MSGQ_DEFINE(ir_tx_queue, sizeof(struct ir_request), LINKR_IR_MAX_QUEUED, 4);

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
/* One replay at a time: the transmit thread loads the frame it is about to
 * send from NVS into these two buffers, so no saved frame is resident. */
static uint8_t ir_play_blob[LINKR_IR_LEARN_BLOB_MAX];
static struct linkr_ir_learn ir_play_frame;
#endif

static void ir_carrier(bool on)
{
	uint32_t pulse = on ? (ir_pwm.period / 3u) : 0u;

	/* pwm_set_pulse_dt() stops the channel (idle low) when the pulse is 0,
	 * which is exactly the "carrier off" state for the IR LED gate. */
	(void)pwm_set_pulse_dt(&ir_pwm, pulse);
}

static void ir_send_nec(const struct ir_request *request)
{
	uint32_t durations[LINKR_NEC_MAX_DURATIONS];
	size_t count;
	uint32_t tail_gap_us;

	count = linkr_nec_encode(request->address, request->command, durations,
				 ARRAY_SIZE(durations));
	if (count == 0) {
		return;
	}

	/* The encoder ends with the inter-frame gap; leave the carrier off for
	 * it instead of busy-waiting through it. */
	tail_gap_us = durations[count - 1u];
	for (size_t i = 0; i < count - 1u; i++) {
		ir_carrier((i % 2u) == 0u);
		k_busy_wait(durations[i]);
	}
	ir_carrier(false);
	k_msleep(DIV_ROUND_UP(tail_gap_us, 1000u));

	for (uint8_t repeat = 0; repeat < request->repeats; repeat++) {
		k_msleep(LINKR_IR_REPEAT_GAP_MS);
		ir_carrier(true);
		k_busy_wait(LINKR_NEC_LEADER_MARK_US);
		ir_carrier(false);
		k_busy_wait(LINKR_NEC_REPEAT_SPACE_US);
		ir_carrier(true);
		k_busy_wait(LINKR_NEC_BIT_MARK_US);
		ir_carrier(false);
	}

	LOG_INF("IR sent addr=0x%04x cmd=0x%02x repeats=%u", request->address,
		request->command, request->repeats);
}

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
static void ir_send_raw(uint8_t slot)
{
	struct linkr_ir_learn *frame = &ir_play_frame;
	char key[24];
	ssize_t length;

	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return;
	}

	/* The cooperative thread is the only reader of these buffers. */
	snprintk(key, sizeof(key), "linkr_ir/slot%u", slot);
	length = settings_load_one(key, ir_play_blob, sizeof(ir_play_blob));
	if (length <= 0) {
		LOG_WRN("IR play slot=%u: empty", slot);
		return;
	}
	if (linkr_ir_learn_parse(frame, ir_play_blob, (size_t)length) != 0) {
		LOG_WRN("IR play slot=%u: stored frame is invalid", slot);
		return;
	}

	for (uint16_t i = 0; i < frame->count; i++) {
		uint32_t duration_us = frame->durations[i];

		if ((i % 2u) == 0u) {
			ir_carrier(true);
			k_busy_wait(duration_us);
		} else {
			ir_carrier(false);
			if (duration_us >= LINKR_IR_LONG_SPACE_US) {
				k_sleep(K_USEC(duration_us));
			} else {
				k_busy_wait(duration_us);
			}
		}
	}
	ir_carrier(false);

	LOG_INF("IR replayed slot=%u edges=%u", slot, frame->count);
}
#endif /* CONFIG_LINKR_BLE_BRIDGE_IR_LEARN */

static void ir_tx_thread(void *arg1, void *arg2, void *arg3)
{
	struct ir_request request;

	ARG_UNUSED(arg1);
	ARG_UNUSED(arg2);
	ARG_UNUSED(arg3);

	(void)k_sem_take(&ir_tx_init_sem, K_FOREVER);

	while (true) {
		(void)k_msgq_get(&ir_tx_queue, &request, K_FOREVER);
		if (request.kind == IR_REQUEST_RAW) {
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
			ir_send_raw(request.slot);
#endif
		} else {
			ir_send_nec(&request);
		}
	}
}

K_THREAD_DEFINE(linkr_ir_tx_thread, LINKR_IR_TX_THREAD_STACK, ir_tx_thread,
		NULL, NULL, NULL, LINKR_IR_TX_THREAD_PRIORITY, 0, 0);

#endif /* CONFIG_LINKR_BLE_BRIDGE_IR_TX */

static uint32_t ir_cycles_to_us(uint32_t cycles)
{
#if CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC >= 1000000
	return cycles / (CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC / 1000000u);
#else
	return (uint32_t)(((uint64_t)cycles * 1000000u) /
			  CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC);
#endif
}

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)

/* Interrupt side of the capture: collect raw intervals until the command
 * layer or the silence detector closes the frame. */
static void ir_learn_isr_edge(int level, uint32_t now, uint32_t elapsed_us)
{
	enum linkr_ir_learn_step step;

	ir_learn_last_cycles = now;

	step = linkr_ir_learn_feed(&learn_stage, level != 0, elapsed_us);
	if (step == LINKR_IR_LEARN_STEP_OVERFLOW ||
	    step == LINKR_IR_LEARN_STEP_BROKEN) {
		ir_learning = false;
		ir_learn_result = step;
		ir_learn_commit_pending = true;
		/* The poll work is already armed while a capture is running; it will
		 * report this within LINKR_IR_LEARN_POLL_MS. */
	}
}

/* Thread side: close the capture and persist it. Silence is detected here
 * because the receiver interrupt only runs on edges: a completed frame is
 * followed by silence, not by another edge. */
static void ir_learn_poll(struct k_work *work)
{
	struct linkr_ir_learn capture;
	enum linkr_ir_learn_step result;
	k_spinlock_key_t key;
	uint8_t slot;
	bool pending;
	bool learning;
	uint32_t now;

	ARG_UNUSED(work);

	key = k_spin_lock(&ir_lock);
	pending = ir_learn_commit_pending;
	learning = ir_learning;
	slot = ir_learn_slot;
	if (!pending && !learning) {
		k_spin_unlock(&ir_lock, key);
		return;
	}

	now = k_cycle_get_32();
	result = ir_learn_result;
	pending = ir_learn_commit_pending;
	ir_learn_commit_pending = false;

	if (pending) {
		/* The interrupt already ended the capture (overflow or lost
		 * pairing); just report it. */
		learning = false;
		ir_learning = false;
		capture = learn_stage;
	} else {
		uint32_t quiet_us = ir_cycles_to_us(now - ir_learn_last_cycles);
		uint32_t age_us = ir_cycles_to_us(now - ir_learn_start_cycles);

		if (ir_learn_started &&
		    quiet_us >= (uint32_t)CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_IDLE_MS *
					1000u) {
			ir_learning = false;
			learning = false;
			capture = learn_stage;
			result = LINKR_IR_LEARN_STEP_READY;
		} else if (age_us >=
			   (uint32_t)CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_TIMEOUT_MS *
				   1000u) {
			ir_learning = false;
			learning = false;
			result = LINKR_IR_LEARN_STEP_IDLE;
		} else {
			result = LINKR_IR_LEARN_STEP_RUNNING;
		}
	}
	k_spin_unlock(&ir_lock, key);

	if (result == LINKR_IR_LEARN_STEP_RUNNING) {
		(void)k_work_reschedule(&ir_learn_poll_work,
					K_MSEC(LINKR_IR_LEARN_POLL_MS));
		return;
	}

	switch (result) {
	case LINKR_IR_LEARN_STEP_READY: {
		uint8_t blob[LINKR_IR_LEARN_BLOB_MAX];
		char name[24];
		size_t length;
		int err;

		if (linkr_ir_learn_finish(&capture) != LINKR_IR_LEARN_STEP_READY) {
			/* Noise or a half frame closed the capture. Keep the session
			 * open until the timeout so the user can press the key again. */
			uint32_t age_us =
				ir_cycles_to_us(k_cycle_get_32() - ir_learn_start_cycles);

			if (age_us <
			    (uint32_t)CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_TIMEOUT_MS *
				    1000u) {
				key = k_spin_lock(&ir_lock);
				ir_learning = true;
				ir_learn_started = false;
				linkr_ir_learn_begin(&learn_stage);
				ir_learn_last_cycles = k_cycle_get_32();
				k_spin_unlock(&ir_lock, key);
				(void)k_work_reschedule(
					&ir_learn_poll_work,
					K_MSEC(LINKR_IR_LEARN_POLL_MS));
				break;
			}
			LOG_WRN("IR learn slot=%u: nothing usable before the timeout",
				slot);
			break;
		}

		length = linkr_ir_learn_serialize(&capture, blob, sizeof(blob));
		if (length == 0) {
			LOG_WRN("IR learn slot=%u: frame too large", slot);
			break;
		}

		snprintk(name, sizeof(name), "linkr_ir/slot%u", slot);
		err = settings_save_one(name, blob, length);
		if (err) {
			LOG_WRN("IR learn slot=%u: save failed: %d", slot, err);
			break;
		}

		key = k_spin_lock(&ir_lock);
		ir_slot_edge_counts[slot] = capture.count;
		k_spin_unlock(&ir_lock, key);
		LOG_INF("IR learned slot=%u edges=%u duration=%ums", slot,
			capture.count,
			linkr_ir_learn_total_us(&capture) / 1000u);
		break;
	}
	case LINKR_IR_LEARN_STEP_OVERFLOW:
		LOG_WRN("IR learn slot=%u: more than %d edges, ignored", slot,
			LINKR_IR_LEARN_MAX_EDGES);
		break;
	case LINKR_IR_LEARN_STEP_BROKEN:
		LOG_WRN("IR learn slot=%u: edges lost, ignored", slot);
		break;
	case LINKR_IR_LEARN_STEP_IDLE:
	default:
		LOG_WRN("IR learn slot=%u: nothing captured before the timeout",
			slot);
		break;
	}
}

#if IS_ENABLED(CONFIG_SETTINGS)
/* Persisted learned commands: the waveform and its label both live in NVS.
 * Boot only validates the fixed header and remembers the edge count; the blob
 * itself is read when a replay needs it. */
static int ir_learn_settings_set(const char *name, size_t len,
				 settings_read_cb read_cb, void *cb_arg)
{
	uint8_t header[LINKR_IR_LEARN_BLOB_HEADER];
	uint32_t slot;
	uint16_t edges;
	bool is_name = false;
	bool valid;

	if (!strncmp(name, "name", 4) && name[4] != '\0') {
		is_name = true;
	} else if (strncmp(name, "slot", 4) != 0 || name[4] == '\0') {
		return -ENOENT;
	}

	slot = 0;
	for (const char *cursor = name + 4; *cursor != '\0'; cursor++) {
		if (*cursor < '0' || *cursor > '9') {
			return -ENOENT;
		}
		slot = (slot * 10u) + (uint32_t)(*cursor - '0');
		if (slot >= LINKR_IR_LEARN_SLOTS) {
			return -ENOENT;
		}
	}

	if (is_name) {
		/* Labels have no boot state; the value is read on demand. */
		return (len != 0 &&
			len <= CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX)
			       ? 0
			       : -EINVAL;
	}

	if (len < sizeof(header) + LINKR_IR_LEARN_BLOB_TRAILER ||
	    len > LINKR_IR_LEARN_BLOB_MAX) {
		return -EINVAL;
	}
	if (read_cb(cb_arg, header, sizeof(header)) != sizeof(header)) {
		return -EINVAL;
	}
	valid = linkr_ir_learn_header_edges(header, len, &edges);
	if (!valid) {
		LOG_WRN("IR learned slot%u: stored frame is invalid", slot);
		return -EINVAL;
	}

	k_spinlock_key_t key = k_spin_lock(&ir_lock);

	ir_slot_edge_counts[slot] = edges;
	k_spin_unlock(&ir_lock, key);
	return 0;
}

SETTINGS_STATIC_HANDLER_DEFINE(linkr_ir_learn, "linkr_ir", NULL,
			       ir_learn_settings_set, NULL, NULL);
#endif /* CONFIG_SETTINGS */

#endif /* CONFIG_LINKR_BLE_BRIDGE_IR_LEARN */

static void ir_rx_isr(const struct device *port, struct gpio_callback *cb,
		      gpio_port_pins_t pins)
{
	struct linkr_nec_frame frame;
	enum linkr_nec_event event;
	uint32_t now = k_cycle_get_32();
	uint32_t elapsed;
	k_spinlock_key_t key;
	int level;

	ARG_UNUSED(cb);
	ARG_UNUSED(pins);

	/* Raw level: the binding marks the line ACTIVE_LOW, but here the edge
	 * direction is what tells a mark interval from a space interval. */
	level = gpio_pin_get_raw(port, ir_rx.pin);

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
	if (ir_learning) {
		/* Learning owns the edge stream. It must run before the NEC
		 * baseline below, because the first falling edge is the mark that
		 * starts the captured frame: consuming it as a baseline would
		 * throw away the frame's leader. */
		if (!ir_learn_started) {
			ir_learn_last_cycles = now;
			if (level == 0) {
				ir_learn_started = true;
			}
			ir_last_valid = false;
			return;
		}

		elapsed = now - ir_learn_last_cycles;
		ir_learn_isr_edge(level, now, ir_cycles_to_us(elapsed));
		ir_last_valid = false;
		return;
	}
#endif

	if (!ir_last_valid) {
		ir_last_valid = true;
		ir_last_cycles = now;
		return;
	}

	elapsed = now - ir_last_cycles;
	ir_last_cycles = now;

	if (ir_cycles_to_us(elapsed) > LINKR_IR_RX_IDLE_RESET_US) {
		linkr_nec_decoder_reset(&ir_decoder);
		return;
	}

	/* A rising edge ends a mark (receiver low while carrier present). */
	event = linkr_nec_feed(&ir_decoder, level != 0,
			       ir_cycles_to_us(elapsed), &frame);
	if (event == LINKR_NEC_EVENT_NONE) {
		return;
	}

	key = k_spin_lock(&ir_lock);
	if (event == LINKR_NEC_EVENT_FRAME) {
		ir_counters.have_frame = true;
		ir_counters.last = frame;
		ir_counters.frames++;
	} else {
		ir_counters.repeats++;
	}
	ir_counters.glitches = ir_decoder.glitches;
	k_spin_unlock(&ir_lock, key);
}

bool linkr_ir_available(void)
{
	return true;
}

static int ir_rx_enable(bool enable)
{
#if !IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_RX)
	if (enable) {
		return -ENOTSUP;
	}
#endif
	int err = gpio_pin_interrupt_configure_dt(
		&ir_rx, enable ? GPIO_INT_EDGE_BOTH : GPIO_INT_DISABLE);

	if (err) {
		return err;
	}

	ir_last_valid = false;
	if (!enable) {
		linkr_nec_decoder_reset(&ir_decoder);
	}
	ir_rx_enabled = enable;
	ir_counters.rx_enabled = enable;
	return 0;
}

int linkr_ir_init(void)
{
	int err;

	if (!device_is_ready(ir_rx.port)) {
		return -ENODEV;
	}

	err = gpio_pin_configure_dt(&ir_rx, GPIO_INPUT);
	if (err) {
		return err;
	}

	gpio_init_callback(&ir_rx_cb, ir_rx_isr, BIT(ir_rx.pin));
	err = gpio_add_callback(ir_rx.port, &ir_rx_cb);
	if (err) {
		return err;
	}

	linkr_nec_decoder_reset(&ir_decoder);
	ir_last_valid = false;
	ir_ready = true;
	ir_counters.rx_enabled = false;

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
	k_work_init_delayable(&ir_learn_poll_work, ir_learn_poll);
#endif

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_RX)
	err = ir_rx_enable(true);
	if (err) {
		LOG_WRN("IR receiver interrupt setup failed: %d", err);
	}
#endif

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_TX) && DT_NODE_HAS_PROP(LINKR_IR_NODE, pwms)
	if (!pwm_is_ready_dt(&ir_pwm)) {
		LOG_WRN("IR carrier PWM device is not ready");
	} else {
		/* Start with the carrier off so the IR LED stays dark. */
		(void)pwm_set_pulse_dt(&ir_pwm, 0);
		ir_tx_ready = true;
		k_sem_give(&ir_tx_init_sem);
	}
#endif

	LOG_INF("IR transceiver ready (rx=%s tx=%s learn=%s)",
		IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_RX) ? "on" : "off",
		IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_TX) ? "on" : "off",
		IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN) ? "on" : "off");
	return 0;
}

int linkr_ir_send(uint16_t address, uint8_t command, uint8_t repeats)
{
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_TX) && DT_NODE_HAS_PROP(LINKR_IR_NODE, pwms)
	struct ir_request request = {
		.kind = IR_REQUEST_NEC,
		.address = address,
		.command = command,
		.repeats = repeats,
	};

	if (!ir_ready || !ir_tx_ready) {
		return -ENODEV;
	}

	return k_msgq_put(&ir_tx_queue, &request, K_NO_WAIT);
#else
	ARG_UNUSED(address);
	ARG_UNUSED(command);
	ARG_UNUSED(repeats);
	return -ENODEV;
#endif
}

int linkr_ir_set_rx(bool enabled)
{
	if (!ir_ready) {
		return -ENODEV;
	}

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
	if (!enabled && ir_learning) {
		/* The capture needs the interrupt; cancel it rather than leaving a
		 * session that can never finish. */
		k_spinlock_key_t key = k_spin_lock(&ir_lock);

		ir_learning = false;
		ir_learn_commit_pending = false;
		k_spin_unlock(&ir_lock, key);
		LOG_WRN("IR learning cancelled: receiver disabled");
	}
#endif

	return ir_rx_enable(enabled);
}

#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN) && \
	IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_TX) && \
	DT_NODE_HAS_PROP(LINKR_IR_NODE, pwms)

int linkr_ir_learn_start(uint8_t slot)
{
	k_spinlock_key_t key;
	uint32_t now;

	if (!ir_ready || !ir_tx_ready) {
		return -ENODEV;
	}
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}

	now = k_cycle_get_32();
	key = k_spin_lock(&ir_lock);
	ir_learning = true;
	ir_learn_started = false;
	ir_learn_commit_pending = false;
	ir_learn_slot = slot;
	ir_learn_result = LINKR_IR_LEARN_STEP_IDLE;
	ir_learn_start_cycles = now;
	ir_learn_last_cycles = now;
	linkr_ir_learn_begin(&learn_stage);
	k_spin_unlock(&ir_lock, key);

	ir_last_valid = false;
	(void)ir_rx_enable(true);
	(void)k_work_reschedule(&ir_learn_poll_work,
				K_MSEC(LINKR_IR_LEARN_POLL_MS));
	LOG_INF("IR learning slot=%u: press the remote key", slot);
	return 0;
}

int linkr_ir_learn_stop(void)
{
	struct linkr_ir_learn capture;
	k_spinlock_key_t key;
	bool learning;

	key = k_spin_lock(&ir_lock);
	learning = ir_learning;
	if (learning) {
		ir_learning = false;
		ir_learn_commit_pending = false;
		capture = learn_stage;
	}
	k_spin_unlock(&ir_lock, key);

	if (!learning) {
		return -EINVAL;
	}

	if (linkr_ir_learn_finish(&capture) != LINKR_IR_LEARN_STEP_READY) {
		return -EAGAIN;
	}

	/* Same commit path as the silence detector. */
	uint8_t blob[LINKR_IR_LEARN_BLOB_MAX];
	char name[24];
	size_t length = linkr_ir_learn_serialize(&capture, blob, sizeof(blob));
	int err;

	if (length == 0) {
		return -EAGAIN;
	}

	snprintk(name, sizeof(name), "linkr_ir/slot%u", ir_learn_slot);
	err = settings_save_one(name, blob, length);
	if (err) {
		LOG_WRN("IR learn slot=%u: save failed: %d", ir_learn_slot, err);
		return -EIO;
	}

	key = k_spin_lock(&ir_lock);
	ir_slot_edge_counts[ir_learn_slot] = capture.count;
	k_spin_unlock(&ir_lock, key);

	LOG_INF("IR learned slot=%u edges=%u", ir_learn_slot, capture.count);
	return 0;
}

int linkr_ir_play(uint8_t slot)
{
	struct ir_request request = {
		.kind = IR_REQUEST_RAW,
		.slot = slot,
	};
	k_spinlock_key_t key;
	bool valid;

	if (!ir_ready || !ir_tx_ready) {
		return -ENODEV;
	}
	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}

	key = k_spin_lock(&ir_lock);
	valid = ir_slot_edge_counts[slot] != 0u;
	k_spin_unlock(&ir_lock, key);

	if (!valid) {
		return -ENOENT;
	}

	return k_msgq_put(&ir_tx_queue, &request, K_NO_WAIT);
}

int linkr_ir_forget(uint8_t slot)
{
	char name[24];
	k_spinlock_key_t key;
	bool had_frame;

	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}

	key = k_spin_lock(&ir_lock);
	had_frame = ir_slot_edge_counts[slot] != 0u;
	ir_slot_edge_counts[slot] = 0u;
	k_spin_unlock(&ir_lock, key);

	if (!had_frame) {
		return -ENOENT;
	}

	snprintk(name, sizeof(name), "linkr_ir/slot%u", slot);
	(void)settings_delete(name);
	snprintk(name, sizeof(name), "linkr_ir/name%u", slot);
	(void)settings_delete(name);

	LOG_INF("IR forgot slot=%u", slot);
	return 0;
}

/* Labels are only touched by the command layer, so they live in NVS alone:
 * no RAM copy, no boot-time load. */
int linkr_ir_slot_name_set(uint8_t slot, const char *name)
{
	char settings_name[24];
	size_t length;
	int err;

	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}

	length = (name == NULL) ? 0u : strlen(name);
	if (length > CONFIG_LINKR_BLE_BRIDGE_IR_LEARN_NAME_MAX) {
		return -EINVAL;
	}

	snprintk(settings_name, sizeof(settings_name), "linkr_ir/name%u",
		 slot);
	if (length == 0u) {
		(void)settings_delete(settings_name);
		return 0;
	}

	err = settings_save_one(settings_name, name, length);
	if (err) {
		LOG_WRN("IR slot=%u: name save failed: %d", slot, err);
		return -EIO;
	}
	LOG_INF("IR slot=%u: name set (%u bytes)", slot, (unsigned int)length);
	return 0;
}

int linkr_ir_slot_name_get(uint8_t slot, char *out, size_t out_len)
{
	char settings_name[24];
	ssize_t length;

	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return -EINVAL;
	}
	if (out == NULL || out_len == 0u) {
		return -ENOSPC;
	}

	snprintk(settings_name, sizeof(settings_name), "linkr_ir/name%u",
		 slot);
	length = settings_load_one(settings_name, out, out_len - 1u);
	if (length == -ENOENT) {
		out[0] = '\0';
		return 0;
	}
	if (length < 0) {
		out[0] = '\0';
		return (int)length;
	}
	if ((size_t)length >= out_len) {
		out[0] = '\0';
		return -ENOSPC;
	}
	out[length] = '\0';
	return (int)length;
}

#else /* !IR_LEARN || !IR_TX */

int linkr_ir_learn_start(uint8_t slot)
{
	ARG_UNUSED(slot);
	return -ENODEV;
}

int linkr_ir_learn_stop(void)
{
	return -ENODEV;
}

int linkr_ir_play(uint8_t slot)
{
	ARG_UNUSED(slot);
	return -ENODEV;
}

int linkr_ir_forget(uint8_t slot)
{
	ARG_UNUSED(slot);
	return -ENODEV;
}

int linkr_ir_slot_name_set(uint8_t slot, const char *name)
{
	ARG_UNUSED(slot);
	ARG_UNUSED(name);
	return -ENODEV;
}

int linkr_ir_slot_name_get(uint8_t slot, char *out, size_t out_len)
{
	ARG_UNUSED(slot);
	ARG_UNUSED(out);
	ARG_UNUSED(out_len);
	return -ENODEV;
}

#endif /* IR_LEARN && IR_TX */

uint16_t linkr_ir_slot_edges(uint8_t slot)
{
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
	k_spinlock_key_t key;
	uint16_t edges;

	if (slot >= LINKR_IR_LEARN_SLOTS) {
		return 0;
	}

	key = k_spin_lock(&ir_lock);
	edges = ir_slot_edge_counts[slot];
	k_spin_unlock(&ir_lock, key);
	return edges;
#else
	ARG_UNUSED(slot);
	return 0;
#endif
}

void linkr_ir_get_status(struct linkr_ir_status *status)
{
	k_spinlock_key_t key;

	if (status == NULL) {
		return;
	}

	key = k_spin_lock(&ir_lock);
	*status = ir_counters;
	status->rx_enabled = ir_rx_enabled;
#if IS_ENABLED(CONFIG_LINKR_BLE_BRIDGE_IR_LEARN)
	status->learning = ir_learning;
	status->learn_slot = ir_learn_slot;
	status->learn_edges = learn_stage.count;
	status->slots_used = 0;
	for (uint8_t slot = 0; slot < LINKR_IR_LEARN_SLOTS; slot++) {
		status->slot_edges[slot] = ir_slot_edge_counts[slot];
		if (status->slot_edges[slot] != 0) {
			status->slots_used++;
		}
	}
#else
	memset(status->slot_edges, 0, sizeof(status->slot_edges));
#endif
	k_spin_unlock(&ir_lock, key);
}

void linkr_ir_clear(void)
{
	k_spinlock_key_t key;

	key = k_spin_lock(&ir_lock);
	memset(&ir_counters, 0, sizeof(ir_counters));
	ir_counters.rx_enabled = ir_rx_enabled;
	k_spin_unlock(&ir_lock, key);

	linkr_nec_decoder_reset(&ir_decoder);
	ir_last_valid = false;
}

#else /* !DT_HAS_ALIAS(linkr_ir) */

bool linkr_ir_available(void)
{
	return false;
}

int linkr_ir_init(void)
{
	return -ENODEV;
}

int linkr_ir_send(uint16_t address, uint8_t command, uint8_t repeats)
{
	ARG_UNUSED(address);
	ARG_UNUSED(command);
	ARG_UNUSED(repeats);
	return -ENODEV;
}

int linkr_ir_set_rx(bool enabled)
{
	ARG_UNUSED(enabled);
	return -ENODEV;
}

int linkr_ir_learn_start(uint8_t slot)
{
	ARG_UNUSED(slot);
	return -ENODEV;
}

int linkr_ir_learn_stop(void)
{
	return -ENODEV;
}

int linkr_ir_play(uint8_t slot)
{
	ARG_UNUSED(slot);
	return -ENODEV;
}

int linkr_ir_forget(uint8_t slot)
{
	ARG_UNUSED(slot);
	return -ENODEV;
}

uint16_t linkr_ir_slot_edges(uint8_t slot)
{
	ARG_UNUSED(slot);
	return 0;
}

void linkr_ir_get_status(struct linkr_ir_status *status)
{
	if (status != NULL) {
		memset(status, 0, sizeof(*status));
	}
}

void linkr_ir_clear(void)
{
}

#endif /* DT_HAS_ALIAS(linkr_ir) */
