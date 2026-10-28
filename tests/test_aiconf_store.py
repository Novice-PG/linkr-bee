"""Cold-start and failed-write regression against the production NVS adapter."""
import os
import re
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
class AiConfigStoreTests(unittest.TestCase):
    def test_cold_start_and_failed_upload(self):
        source = (ROOT / "src/linkr_aiconf.c").read_text()
        source = re.sub(r"^#include.*$", "", source, flags=re.M)
        with tempfile.TemporaryDirectory(prefix="bee-aiconf-") as directory:
            src = Path(directory) / "test.c"
            src.write_text(r'''#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <sys/types.h>
#include <errno.h>
#include <string.h>
#include <assert.h>
#define LINKR_AICONF_MAX 960
#define LOG_MODULE_DECLARE(...)
#define LOG_WRN(...)
#define LOG_INF(...)
static const char stored[] = "{\"v\":1,\"model\":\"saved\"}";
static int loads;
ssize_t settings_get_val_len(const char *key) {(void)key; return sizeof(stored)-1;}
ssize_t settings_load_one(const char *key, void *out, size_t n) {
 (void)key; assert(n >= sizeof(stored)-1); loads++; memcpy(out, stored, sizeof(stored)-1); return sizeof(stored)-1;
}
int settings_save_one(const char *key, const void *data, size_t n) {(void)key;(void)data;(void)n;return -EIO;}
int settings_delete(const char *key) {(void)key;return 0;}
''' + source + r'''
int main(void) {
 uint8_t out[960] = {0};
 assert(linkr_aiconf_len() == sizeof(stored)-1);
 assert(linkr_aiconf_len() == sizeof(stored)-1);
 assert(linkr_aiconf_read(0,out,sizeof(out)) == sizeof(stored)-1);
 assert(memcmp(out,stored,sizeof(stored)-1)==0 && loads==1);
 assert(linkr_aiconf_read(5,out,3)==3 && memcmp(out,stored+5,3)==0 && loads==1);
 assert(linkr_aiconf_begin()==0);
 assert(linkr_aiconf_put((const uint8_t *)"broken",6)==6);
 assert(linkr_aiconf_read(0,out,sizeof(out))==-EBUSY);
 assert(linkr_aiconf_commit()==-EIO);
 assert(linkr_aiconf_len()==sizeof(stored)-1);
 assert(linkr_aiconf_read(0,out,sizeof(out))==sizeof(stored)-1);
 assert(memcmp(out,stored,sizeof(stored)-1)==0 && loads==2);
 return 0;
}
''')
            binary = Path(directory) / "test"
            subprocess.run([*shlex.split(os.environ.get("CC", "cc")), "-std=c11", "-Wall", "-Wextra", "-Werror", str(src), "-o", str(binary)], check=True)
            subprocess.run([str(binary)], check=True, timeout=5)
