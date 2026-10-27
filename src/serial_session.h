/* SPDX-License-Identifier: Apache-2.0 */
#ifndef LINKR_SERIAL_SESSION_H
#define LINKR_SERIAL_SESSION_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum linkr_session_transport {
	LINKR_SESSION_BLE,
	LINKR_SESSION_LAN,
};

int linkr_session_open(const void *peer, enum linkr_session_transport transport);
void linkr_session_close(const void *peer);
/* A nonzero ticket identifies both the owner and this ownership generation. */
uint32_t linkr_session_ticket(const void *peer, bool claim_if_idle);
bool linkr_session_begin(uint32_t ticket);
void linkr_session_end(void);
uint32_t linkr_session_epoch(const void *peer);
bool linkr_session_command_begin(const void *peer, uint32_t queued_epoch);
bool linkr_session_control(const void *peer, const char *command,
			  char *response, size_t capacity);
void linkr_session_status(const void *peer, char *response, size_t capacity);

#endif
