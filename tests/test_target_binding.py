"""Exercise production persistence logic with host settings stubs; not a flash test."""
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

class TargetBindingTests(unittest.TestCase):
    def test_persistence_validation_and_failed_write(self):
        root = Path(__file__).resolve().parents[1]
        production = re.sub(r'^#include[^\n]*\n', '', (root / 'src/target_binding.c').read_text(), flags=re.M)
        source = r'''
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdbool.h>
#include <string.h>
#include <sys/types.h>
#define K_MUTEX_DEFINE(n) int n
#define K_FOREVER 0
#define SETTINGS_STATIC_HANDLER_DEFINE(...)
#define k_mutex_lock(a,b) ((void)0)
#define k_mutex_unlock(a) ((void)0)
typedef ssize_t (*settings_read_cb)(void *, void *, size_t);
static int fail, writes;
static char saved[37];
static int settings_save_one(const char *k,const void *v,size_t n) { (void)k; if(fail)return -EIO; memcpy(saved,v,n);writes++;return 0; }
static int settings_delete(const char *k) { (void)k;if(fail)return -EIO;saved[0]=0;return 0; }
static ssize_t load(void *arg,void *data,size_t n) { (void)arg;memcpy(data,saved,n);return n; }
''' + production + r'''
int main(void) {
 char out[80];
 const char *id="12345678-1234-4123-8123-123456789abc";
 assert(linkr_target_command("target?",out,sizeof(out))); assert(strstr(out,"none"));
 assert(linkr_target_command("target=invalid",out,sizeof(out))); assert(strstr(out,"ERR")); assert(writes==0);
 char command[60];snprintf(command,sizeof(command),"target=%s",id);
 linkr_target_command(command,out,sizeof(out));assert(strstr(out,id));assert(writes==1);
 linkr_target_command(command,out,sizeof(out));assert(writes==1);
 target_id[0]=0;assert(binding_load("v1",37,load,NULL)==0);assert(!strcmp(target_id,id));
 fail=1;linkr_target_command("target clear",out,sizeof(out));assert(strstr(out,"ERR"));assert(!strcmp(target_id,id));
 linkr_target_command("target=aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",out,sizeof(out));assert(strstr(out,"ERR"));assert(!strcmp(target_id,id));
 fail=0;linkr_target_command("target clear",out,sizeof(out));assert(strstr(out,"none"));
 saved[36]='x';assert(binding_load("v1",37,load,NULL)==-EINVAL);
 assert(!linkr_target_command("info?",out,sizeof(out)));
}
'''
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / 'test.c').write_text(source)
            subprocess.run(['cc','-std=c11',str(path/'test.c'),'-o',str(path/'test')],check=True)
            subprocess.run([str(path/'test')],check=True)
