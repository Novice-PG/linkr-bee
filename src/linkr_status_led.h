/*
 * Linkr Bee status LED.
 *
 * Two board generations exist behind one interface:
 *   - the ESP32-C3/C5 dev boards and the Super Mini drive a plain GPIO LED
 *     (led0), activity = a 40 ms pulse, exactly like before;
 *   - the Linkr Bee X1.0 has an addressable RGB LED on GPIO8, where activity
 *     is a short flash in the activity colour while a caller-selected base
 *     colour stays on.
 *
 * Keeping the choice here means main.c and the control commands do not have
 * to know which kind of LED the board has.
 */
#ifndef LINKR_STATUS_LED_H_
#define LINKR_STATUS_LED_H_

#include <stdbool.h>
#include <stdint.h>

/* True when this board can show status at all. */
bool linkr_status_led_available(void);

/* Configure the LED hardware. Safe to call once during init; returns
 * -ENODEV when the board has no status LED. */
int linkr_status_led_init(void);

/* Flash the activity indication (UART traffic). Cheap and safe to call from
 * any thread; it coalesces repeated calls into one flash. */
void linkr_status_led_activity(void);

/* Set the steady colour. On a single-colour LED any non-black value means
 * "on". Returns -ENODEV when the board has no status LED. */
int linkr_status_led_set_rgb(uint8_t r, uint8_t g, uint8_t b);

/* Turn the steady colour off. */
void linkr_status_led_off(void);

/* Read the steady colour. */
void linkr_status_led_get_rgb(uint8_t *r, uint8_t *g, uint8_t *b);

/* Blocking blink used by the UART loopback self-test. */
void linkr_status_led_blink(int count, int delay_ms);

#endif /* LINKR_STATUS_LED_H_ */
