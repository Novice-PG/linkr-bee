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
		} else if (!((id[i] >= '0' && id[i] <= '9') || (id[i] >= 'a' && id[i] <= 'f'))) return false;
	}
	return true;
}
static int binding_load(const char *name, size_t len, settings_read_cb read_cb, void *arg)
{
	char value[37] = {0};
	if (strcmp(name, "v1")) return -ENOENT;
	if (len != sizeof(value)) return -EINVAL;
	int got = read_cb(arg, value, sizeof(value));
	if (got != sizeof(value) || value[36] != '\0' || !valid_id(value)) return -EINVAL;
	memcpy(target_id, value, sizeof(value));
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
		const char *id = command + 7;
		if (!valid_id(id)) err = -EINVAL;
		else if (strcmp(id, target_id)) {
			err = settings_save_one("linkr_target/v1", id, 37);
			if (!err) memcpy(target_id, id, 37);
		}
	}
	if (err) snprintf(response, size, "ERR target storage %d\r\n", err);
	else snprintf(response, size, "OK target=%s\r\n", target_id[0] ? target_id : "none");
	k_mutex_unlock(&target_lock);
	return true;
}
