/* SPDX-License-Identifier: Apache-2.0 */
#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>
#include "serial_session.h"

#define SESSION_COUNT (CONFIG_BT_MAX_CONN + 4)
static struct {
	const void *peer;
	uint32_t id;
	enum linkr_session_transport transport;
	bool managed;
} sessions[SESSION_COUNT];
static uint32_t next_id;
static uint32_t owner;
static uint32_t epoch = 1;
K_MUTEX_DEFINE(session_lock);

static void ownership_changed(void)
{
	if (++epoch == 0) ++epoch;
}

static uint32_t peer_id(const void *peer)
{
	for (size_t i = 0; i < SESSION_COUNT; i++) {
		if (sessions[i].peer == peer) return sessions[i].id;
	}
	return 0;
}

static bool peer_managed(const void *peer)
{
	for (size_t i = 0; i < SESSION_COUNT; i++) {
		if (sessions[i].peer == peer) return sessions[i].managed;
	}
	return true;
}

int linkr_session_open(const void *peer, enum linkr_session_transport transport)
{
	int result = -ENOMEM;
	if (!peer) return -EINVAL;
	k_mutex_lock(&session_lock, K_FOREVER);
	if (peer_id(peer)) { result = 0; goto out; }
	for (size_t i = 0; i < SESSION_COUNT; i++) {
		if (sessions[i].peer) continue;
		if (++next_id == 0) ++next_id;
		sessions[i].peer = peer;
		sessions[i].id = next_id;
		sessions[i].transport = transport;
		result = 0;
		break;
	}
out:
	k_mutex_unlock(&session_lock);
	return result;
}

void linkr_session_close(const void *peer)
{
	k_mutex_lock(&session_lock, K_FOREVER);
	for (size_t i = 0; i < SESSION_COUNT; i++) {
		if (sessions[i].peer != peer) continue;
		if (owner == sessions[i].id) { owner = 0; ownership_changed(); }
		memset(&sessions[i], 0, sizeof(sessions[i]));
	}
	k_mutex_unlock(&session_lock);
}

uint32_t linkr_session_ticket(const void *peer, bool claim_if_idle)
{
	uint32_t ticket = 0;
	k_mutex_lock(&session_lock, K_FOREVER);
	uint32_t id = peer_id(peer);
	if (id && !owner && claim_if_idle && !peer_managed(peer)) { owner = id; ownership_changed(); }
	if (id && owner == id) ticket = epoch;
	k_mutex_unlock(&session_lock);
	return ticket;
}

bool linkr_session_begin(uint32_t ticket)
{
	k_mutex_lock(&session_lock, K_FOREVER);
	if (ticket && owner && ticket == epoch) return true;
	k_mutex_unlock(&session_lock);
	return false;
}

void linkr_session_end(void)
{
	k_mutex_unlock(&session_lock);
}

uint32_t linkr_session_epoch(const void *peer)
{
	k_mutex_lock(&session_lock, K_FOREVER);
	uint32_t result = peer_id(peer) ? epoch : 0;
	k_mutex_unlock(&session_lock);
	return result;
}

bool linkr_session_command_begin(const void *peer, uint32_t queued_epoch)
{
	k_mutex_lock(&session_lock, K_FOREVER);
	uint32_t self = peer_id(peer);
	if (!self || !queued_epoch || queued_epoch != epoch || (owner && owner != self) ||
	    (!owner && peer_managed(peer))) {
		k_mutex_unlock(&session_lock);
		return false;
	}
	if (!owner) { owner = self; ownership_changed(); }
	return true;
}

static bool session_control(const void *peer, const char *command,
			    char *response, size_t capacity, bool managed)
{
	const char *action;
	if (!strcmp(command, "c?") || !strcmp(command, "control?")) action = "?";
	else if (!strncmp(command, "c ", 2)) action = command + 2;
	else if (!strncmp(command, "control ", 8)) action = command + 8;
	else return false;
	k_mutex_lock(&session_lock, K_FOREVER);
	uint32_t self = peer_id(peer);
	for (size_t i = 0; i < SESSION_COUNT; i++) {
		if (sessions[i].peer == peer && managed) sessions[i].managed = true;
	}
	const char *error = NULL;
	if (!self) error = "disconnected";
	else if (!strcmp(action, "claim")) {
		if (owner && owner != self) error = "busy";
		else if (!owner) { owner = self; ownership_changed(); }
	} else if (!strcmp(action, "release")) {
		if (owner != self) error = "not_owner";
		else { owner = 0; ownership_changed(); }
	} else if (!strncmp(action, "yield ", 6)) {
		char *end;
		unsigned long target = strtoul(action + 6, &end, 10);
		bool found = false;
		for (size_t i = 0; i < SESSION_COUNT; i++) {
			if (sessions[i].peer && sessions[i].id == target) found = true;
		}
		if (owner != self) error = "not_owner";
		else if (!target || target > UINT32_MAX || *end || !found) error = "bad_peer";
		else if (owner != target) { owner = target; ownership_changed(); }
	} else if (strcmp(action, "?")) error = "bad_action";
	unsigned int peers = 0;
	for (size_t i = 0; i < SESSION_COUNT; i++) peers += sessions[i].peer != NULL;
	if (error) snprintk(response, capacity, "ERR control=%s\r\n", error);
	else snprintk(response, capacity,
		"OK control self=%u owner=%u mode=%s epoch=%u peers=%u ble_max=%u linkr_reserved=0\r\n",
		self, owner, owner == self ? "control" : owner ? "readonly" : "available",
		epoch, peers, CONFIG_BT_MAX_CONN);
	k_mutex_unlock(&session_lock);
	return true;
}

bool linkr_session_control(const void *peer, const char *command,
			  char *response, size_t capacity)
{
	return session_control(peer, command, response, capacity, true);
}

void linkr_session_status(const void *peer, char *response, size_t capacity)
{
	(void)session_control(peer, "control?", response, capacity, false);
}
