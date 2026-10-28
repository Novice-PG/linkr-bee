/* SPDX-License-Identifier: Apache-2.0 */
#include <assert.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "serial_session.h"

static char a, b, c;
static char response[256];
static void command(const void *peer, const char *text)
{
	assert(linkr_session_control(peer, text, response, sizeof(response)));
}
static unsigned int id(const void *peer)
{
	unsigned int result = 0;
	linkr_session_status(peer, response, sizeof(response));
	assert(sscanf(response, "OK control self=%u", &result) == 1);
	return result;
}
#include "control_dispatch.inc"

static atomic_int start;
static void *claim(void *peer)
{
	char reply[256];
	while (!atomic_load(&start)) { }
	assert(linkr_session_control(peer, "c claim", reply, sizeof(reply)));
	return NULL;
}

int main(int argc, char **argv)
{
	assert(argc == 2);
	assert(!linkr_session_open(&a, LINKR_SESSION_BLE));
	assert(!linkr_session_open(&b, LINKR_SESSION_BLE));
	assert(!linkr_session_open(&c, LINKR_SESSION_LAN));
	if (!strcmp(argv[1], "single_writer")) {
		uint32_t ticket = linkr_session_ticket(&a, true);
		assert(ticket && !linkr_session_ticket(&b, true) && !linkr_session_ticket(&c, true));
		linkr_session_close(&b);
		assert(linkr_session_begin(ticket));
		linkr_session_end();
	} else if (!strcmp(argv[1], "managed_release")) {
		command(&a, "c?");
		assert(strstr(response, "mode=available"));
		assert(!linkr_session_ticket(&a, true));
		command(&a, "c claim");
		uint32_t ticket = linkr_session_ticket(&a, false);
		assert(ticket);
		command(&a, "c release");
		assert(!linkr_session_ticket(&a, true));
		assert(!linkr_session_begin(ticket));
	} else if (!strcmp(argv[1], "handover")) {
		unsigned int target = id(&b);
		command(&a, "c claim");
		uint32_t old = linkr_session_ticket(&a, false);
		char text[64];
		snprintf(text, sizeof(text), "c yield %u", target);
		command(&c, text);
		assert(strstr(response, "not_owner"));
		command(&a, text);
		assert(strstr(response, "mode=readonly"));
		assert(!linkr_session_begin(old));
		assert(!linkr_session_ticket(&a, true));
		assert(linkr_session_ticket(&b, false));
	} else if (!strcmp(argv[1], "reconnect")) {
		uint32_t old = linkr_session_ticket(&a, true);
		unsigned int old_id = id(&a);
		linkr_session_close(&a);
		assert(!linkr_session_open(&a, LINKR_SESSION_BLE));
		assert(id(&a) != old_id);
		assert(linkr_session_ticket(&a, true));
		assert(!linkr_session_begin(old));
	} else if (!strcmp(argv[1], "queued_command")) {
		uint32_t queued = linkr_session_epoch(&a);
		command(&b, "c claim");
		command(&b, "c release");
		assert(!linkr_session_command_begin(&a, queued));
		assert(linkr_session_command_begin(&a, linkr_session_epoch(&a)));
		linkr_session_end();
	} else if (!strcmp(argv[1], "concurrent_claims")) {
		pthread_t first, second;
		assert(!pthread_create(&first, NULL, claim, &a));
		assert(!pthread_create(&second, NULL, claim, &c));
		atomic_store(&start, 1);
		pthread_join(first, NULL);
		pthread_join(second, NULL);
		assert((linkr_session_ticket(&a, false) != 0) != (linkr_session_ticket(&c, false) != 0));
	} else if (!strcmp(argv[1], "disconnected")) {
		linkr_session_close(&a);
		assert(!linkr_session_ticket(&a, true));
		assert(!linkr_session_command_begin(&a, 1));
		command(&a, "c claim");
		assert(strstr(response, "disconnected"));
	} else if (!strcmp(argv[1], "bad_handover")) {
		command(&a, "c claim");
		uint32_t before = linkr_session_ticket(&a, false);
		command(&a, "c yield 9999999999999999999");
		assert(strstr(response, "bad_peer"));
		assert(before == linkr_session_ticket(&a, false));
	} else if (!strcmp(argv[1], "legacy_lan")) {
		linkr_session_status(&c, response, sizeof(response));
		assert(linkr_session_ticket(&c, true));
		assert(!linkr_session_ticket(&a, true));
	} else if (!strcmp(argv[1], "aux_permissions")) {
		command(&a, "c claim");
		uint32_t current = linkr_session_epoch(&a);
		const char *mutations[] = {"o on", "r tx 1,2", "p clear", "y begin"};
		for (size_t i = 0; i < sizeof(mutations)/sizeof(mutations[0]); i++) {
			int before = aux_calls;
			assert(dispatch_aux(&b, mutations[i], current));
			assert(aux_calls == before);
			assert(dispatch_aux(&a, mutations[i], current));
			assert(aux_calls == before + 1);
		}
		assert(dispatch_aux(&b, "p get", current));
		int before = aux_calls;
		command(&a, "c release");
		assert(dispatch_aux(&a, "o on", current));
		assert(aux_calls == before);
	} else abort();
	puts("ok");
	return 0;
}
