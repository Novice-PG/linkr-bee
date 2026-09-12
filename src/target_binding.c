/* Small persistent association; the target must still be verified over UART. */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <zephyr/kernel.h>
#include <zephyr/settings/settings.h>
#include "target_binding.h"

static char target_id[37];
K_MUTEX_DEFINE(target_lock);
static bool valid_id(const char *id)
{
	if (strlen(id) != 36) return false;
	for (int i = 0; i < 36; i++) {
		if (i == 8 || i == 13 || i == 18 || i == 23) {
			if (id[i] != '-') return false;
		} else if (!((id[i] >= '0' && id[i] <= '9') ||
			     (id[i] >= 'a' && id[i] <= 'f') ||
			     (id[i] >= 'A' && id[i] <= 'F'))) return false;
	}
	return true;
}

/* RFC 4122 spells a UUID in either case, but settings_save_one() persists these
 * exact bytes and every later comparison reads them back, so fold the id to the
 * canonical lowercase form before it is stored or compared. The caller has
 * already accepted id with valid_id(). */
static void id_to_lower(char dest[37], const char *id)
{
	for (int i = 0; i < 36; i++) {
		char ch = id[i];

		if (ch >= 'A' && ch <= 'F') ch = (char)(ch - 'A' + 'a');
		dest[i] = ch;
	}
	dest[36] = '\0';
}
static int binding_load(const char *name, size_t len, settings_read_cb read_cb, void *arg)
{
	char value[37] = {0};
	if (strcmp(name, "v1")) return -ENOENT;
	if (len != sizeof(value)) return -EINVAL;
	int got = read_cb(arg, value, sizeof(value));
	if (got != sizeof(value) || value[36] != '\0' || !valid_id(value)) return -EINVAL;
	id_to_lower(target_id, value);
	return 0;
}
SETTINGS_STATIC_HANDLER_DEFINE(linkr_target, "linkr_target", NULL, binding_load, NULL, NULL);

bool linkr_target_command(const char *command, char *response, size_t size)
{
	if (strcmp(command, "target?") && strcmp(command, "target clear") && strncmp(command, "target=", 7)) return false;
	int err = 0;
	k_mutex_lock(&target_lock, K_FOREVER);
	if (!strcmp(command, "target clear")) {
		err = settings_delete("linkr_target/v1");
		if (!err) target_id[0] = '\0';
	} else if (!strncmp(command, "target=", 7)) {
		char id[37];

		if (!valid_id(command + 7)) err = -EINVAL;
		else {
			id_to_lower(id, command + 7);
			if (strcmp(id, target_id)) {
				err = settings_save_one("linkr_target/v1", id, sizeof(id));
				if (!err) memcpy(target_id, id, sizeof(id));
			}
		}
	}
	if (err) snprintf(response, size, "ERR target storage %d\r\n", err);
	else snprintf(response, size, "OK target=%s\r\n", target_id[0] ? target_id : "none");
	k_mutex_unlock(&target_lock);
	return true;
}
