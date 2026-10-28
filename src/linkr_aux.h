/*
 * Linkr Bee auxiliary board I/O: status LED, optocoupler switch, IR remote.
 *
 * The board features live behind one command group so main.c keeps a single
 * dispatch point:
 *
 *   @l?                     current status LED colour
 *   @l=<r>,<g>,<b>          steady colour, channels 0-255 or 0x00-0xff
 *   @l off                  LED dark
 *   @o?                     optocoupler contact state
 *   @o on | @o close        close the J5 dry contact
 *   @o off | @o open        release it
 *   @o pulse <ms>           close for <ms>, then release
 *   @r?                     IR receiver counters and last NEC frame
 *   @r tx <addr>,<cmd>[,<repeats>]
 *                           transmit a NEC frame (address 0-0xffff)
 *   @r rx on|off            enable or disable NEC decoding
 *   @r clear                forget the last frame and counters
 *
 * Long forms are `led`, `opto` and `ir`. Commands for hardware a board does
 * not have answer ERR ... unsupported instead of disappearing, so a client can
 * tell "absent" from "typo".
 */
#ifndef LINKR_AUX_H_
#define LINKR_AUX_H_

#include <stdbool.h>
#include <stddef.h>

/* Initialize the auxiliary hardware this board has. Returns the first error
 * but initializes every device it can: a missing optocoupler must not keep
 * the status LED dark. */
int linkr_aux_init(void);

/* Handle one control command (the body after '@', already trimmed).
 * Returns true when the command belongs to this group and `response` holds the
 * line to send back (with CRLF). */
/* Exact read-only commands; all other auxiliary requests require ownership. */
bool linkr_aux_is_readonly(const char *body);

bool linkr_aux_command(const char *body, char *response, size_t response_size);

/* Largest help fragment this module can produce. */
#define LINKR_AUX_HELP_MAX 512

/* Write the help fragment for the commands this board actually supports into
 * `buffer`. Built on demand instead of kept in a static RAM string; the @h
 * handler is the only caller. */
void linkr_aux_help(char *buffer, size_t size);

#endif /* LINKR_AUX_H_ */
