/*
 * Linkr Bee X1.0 optocoupler switch.
 *
 * Q1 (HT_354_ATP1) is wired as an isolated dry contact across J5 pins 1-2.
 * Its LED is fed from +3V3 through R13 510R and pulled to ground by GPIO10,
 * so driving the pin low closes the contact. That makes the J5 pair behave
 * like a remote button on whatever is connected to it (typically the reset or
 * power key of the board whose console this bridge carries).
 */
#ifndef LINKR_OPTO_H_
#define LINKR_OPTO_H_

#include <stdbool.h>
#include <stdint.h>

/* True when this board has the optocoupler switch. */
bool linkr_opto_available(void);

/* Configure the control pin with the contact open. */
int linkr_opto_init(void);

/* Close (true) or open (false) the isolated contact. */
int linkr_opto_set(bool closed);

/* Close the contact for `duration_ms`, then release it. */
int linkr_opto_pulse(uint32_t duration_ms);

/* Last requested state. */
bool linkr_opto_is_closed(void);

#endif /* LINKR_OPTO_H_ */
