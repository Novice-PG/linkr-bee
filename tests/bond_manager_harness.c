#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <sys/types.h>
#define ARG_UNUSED(x) (void)(x)
#define K_MUTEX_DEFINE(x) int x
#define K_FOREVER 0
#define K_MSEC(x) (x)
#define BT_SECURITY_L2 2
#define BT_ADDR_STR_LEN 18
#define snprintk snprintf
struct k_work { int unused; };
struct k_work_delayable { void (*fn)(struct k_work *); };
#define K_WORK_DELAYABLE_DEFINE(n,f) struct k_work_delayable n = {f}
static int k_mutex_lock(int *m,int t){(void)m;(void)t;return 0;}
static void k_mutex_unlock(int *m){(void)m;}
static int schedule_error;
static int k_work_reschedule(struct k_work_delayable *w,int t){(void)w;assert(t==500);return schedule_error;}
typedef struct { char text[18]; } bt_addr_t;
typedef struct { uint8_t type; bt_addr_t a; } bt_addr_le_t;
struct bt_bond_info { bt_addr_le_t addr; };
struct bt_conn { int unused; };
struct bt_conn_info { uint8_t id; };
static bt_addr_le_t self={0,{"AA:BB:CC:DD:EE:01"}};
static bool exists=true;
static int security=2, removals;
static int bt_conn_get_info(struct bt_conn*c,struct bt_conn_info*i){(void)c;i->id=0;return 0;}
static int bt_conn_get_security(struct bt_conn*c){(void)c;return security;}
static const bt_addr_le_t *bt_conn_get_dst(struct bt_conn*c){(void)c;return &self;}
static bool bt_addr_le_eq(const bt_addr_le_t*a,const bt_addr_le_t*b){return a->type==b->type&&!strcmp(a->a.text,b->a.text);}
static void bt_addr_le_copy(bt_addr_le_t*a,const bt_addr_le_t*b){*a=*b;}
static int bt_addr_to_str(const bt_addr_t*a,char*b,size_t n){return snprintf(b,n,"%s",a->text);}
static int bt_addr_le_from_str(const char*s,const char*t,bt_addr_le_t*a){if(strcmp(s,self.a.text))return -EINVAL;a->type=!strcmp(t,"random");snprintf(a->a.text,18,"%s",s);return 0;}
static bool bt_le_bond_exists(uint8_t id,const bt_addr_le_t*a){assert(id==0);return exists&&bt_addr_le_eq(a,&self);}
static int bt_unpair(uint8_t id,const bt_addr_le_t*a){assert(id==0&&bt_addr_le_eq(a,&self));exists=false;removals++;return 0;}
static void bt_foreach_bond(uint8_t id,void(*cb)(const struct bt_bond_info*,void*),void*data){assert(id==0);if(exists){struct bt_bond_info b={self};cb(&b,data);}}

/* In-memory settings store: the label blob is the only key this module writes. */
static uint8_t nvs[1024];
static size_t nvs_len;
static int save_fail, saves;
static ssize_t settings_load_one(const char *key, void *buf, size_t len)
{
    if (strcmp(key, "linkr_bond/labels") != 0) return -2 /* -ENOENT */;
    if (nvs_len == 0 || nvs_len > len) return -2;
    memcpy(buf, nvs, nvs_len);
    return (ssize_t)nvs_len;
}
static int settings_save_one(const char *key, const void *buf, size_t len)
{
    assert(!strcmp(key, "linkr_bond/labels"));
    assert(len <= sizeof(nvs));
    if (save_fail) return -5 /* -EIO */;
    saves++;
    memcpy(nvs, buf, len);
    nvs_len = len;
    return 0;
}
#include "bond_production.inc"
int main(void){
 struct bt_conn conn={0};char out[512];
 assert(!linkr_bond_command(&conn,"other",out,sizeof out));
 linkr_bond_command(NULL,"bonds?",out,sizeof out);assert(strstr(out,"ERR"));
 security=1;linkr_bond_command(&conn,"bonds remove 0/AA:BB:CC:DD:EE:01",out,sizeof out);assert(strstr(out,"ERR")&&!pending);security=2;
 linkr_bond_command(&conn,"bonds?",out,sizeof out);assert(strstr(out,"BOND 0/AA:BB:CC:DD:EE:01 self=1"));
 linkr_bond_command(&conn,"bonds?",out,12);assert(strstr(out,"ERR"));
 const char *bad[]={"bonds remove all","bonds remove 2/AA:BB:CC:DD:EE:01","bonds remove 0/AA:BB:CC:DD:EE:01 extra","bonds remove 1/AA:BB:CC:DD:EE:01"};
 for(size_t i=0;i<sizeof bad/sizeof bad[0];i++){linkr_bond_command(&conn,bad[i],out,sizeof out);assert(strstr(out,"ERR")&&!pending);}
 schedule_error=-ENOMEM;linkr_bond_command(&conn,"bonds remove 0/AA:BB:CC:DD:EE:01",out,sizeof out);assert(!pending&&strstr(out,"ERR"));schedule_error=0;
 linkr_bond_command(&conn,"bonds remove 0/AA:BB:CC:DD:EE:01",out,sizeof out);assert(pending&&!removals&&strstr(out,"OK bonds queued"));
 linkr_bond_command(&conn,"bonds remove 0/AA:BB:CC:DD:EE:01",out,sizeof out);assert(strstr(out,"busy"));
 linkr_bond_command(&conn,"bonds?",out,sizeof out);assert(strstr(out,"pending=1"));
 remove_work.fn(NULL);assert(!pending&&removals==1&&!exists);
 linkr_bond_command(&conn,"bonds?",out,sizeof out);assert(strstr(out,"pending=0 result=0")&&!strstr(out,"BOND "));
 linkr_bond_command(&conn,"bonds remove 0/AA:BB:CC:DD:EE:01",out,sizeof out);assert(strstr(out,"not found"));

 /* ---- host labels ----------------------------------------------------- */
 exists=true;
 /* label_clean drops what would break the listing, collapses spaces and never
  * leaves half a UTF-8 character behind. */
 {char clean[LABEL_MAX+1];
  assert(label_clean("  longlong\t\"mac\"  ",clean,sizeof clean)==12);
  assert(!strcmp(clean,"longlong mac"));
  assert(label_clean("   ",clean,sizeof clean)==0);
  /* "长龙" is 6 bytes; cap 5 cannot hold the second character. */
  assert(label_clean("\xe9\x95\xbf\xe9\xbe\x99",clean,5)==3);
  assert(!strcmp(clean,"\xe9\x95\xbf"));
  assert(label_clean("\xe9\x95\xbf\xe9\xbe\x99",clean,sizeof clean)==6);
  assert(!strcmp(clean,"\xe9\x95\xbf\xe9\xbe\x99"));}

 /* name_line quotes the label so spaces survive, in the BOND id form. */
 {char line[96];
  assert(name_line(line,sizeof line,0,"AA:BB:CC:DD:EE:01","longlong 的 iPhone")>0);
  assert(strstr(line,"NAME 0/AA:BB:CC:DD:EE:01 \"longlong 的 iPhone\""));}

 /* An empty name is a usage error, not a cleared label. */
 linkr_bond_command(&conn,"bonds name",out,sizeof out);assert(strstr(out,"ERR bonds name length"));
 linkr_bond_command(&conn,"bonds name    ",out,sizeof out);assert(strstr(out,"ERR bonds name length"));

 /* Setting the connected peer's label, then reading it back. */
 linkr_bond_command(&conn,"bonds name longlong-mbp",out,sizeof out);assert(strstr(out,"OK bonds name"));
 linkr_bond_command(&conn,"bonds names?",out,sizeof out);assert(strstr(out,"OK bonds names"));
 assert(strstr(out,"NAME 0/AA:BB:CC:DD:EE:01 \"longlong-mbp\""));
 /* The plain listing keeps its old shape, so an older client still parses it. */
 linkr_bond_command(&conn,"bonds?",out,sizeof out);
 assert(strstr(out,"BOND 0/AA:BB:CC:DD:EE:01 self=1")&&!strstr(out,"NAME "));

 /* Re-selecting the same peer overwrites instead of adding a second entry. */
 linkr_bond_command(&conn,"bonds name second-name",out,sizeof out);
 assert(strstr(out,"OK bonds name")&&label_blob[1]==1);

 /* Survives a reboot: the blob is in settings, the RAM cache is not. */
 label_ready=false;
 linkr_bond_command(&conn,"bonds names?",out,sizeof out);
 assert(strstr(out,"NAME 0/AA:BB:CC:DD:EE:01 \"second-name\""));

 /* A label whose bond is gone is dropped when the list is asked for. */
 assert(label_set(1,"11:22:33:44:55:66","ghost")==0&&label_blob[1]==2);
 linkr_bond_command(&conn,"bonds names?",out,sizeof out);
 assert(!strstr(out,"ghost")&&label_blob[1]==1&&label_find(1,"11:22:33:44:55:66")<0);

 /* Small buffers are refused rather than truncated mid-line. A buffer that
  * cannot even hold the error text still has to say ERR, like bonds? does. */
 linkr_bond_command(&conn,"bonds names?",out,12);assert(strstr(out,"ERR"));
 linkr_bond_command(&conn,"bonds names?",out,32);assert(strstr(out,"ERR bonds buffer"));

 /* A failed settings write is reported, and the cache does not keep the change:
  * reading back must still show the last stored name. */
 save_fail=1;
 linkr_bond_command(&conn,"bonds name write-fail",out,sizeof out);
 assert(strstr(out,"ERR bonds name=-5"));
 save_fail=0;
 linkr_bond_command(&conn,"bonds names?",out,sizeof out);
 assert(!strstr(out,"write-fail")&&strstr(out,"second-name"));

 /* Labels only exist for bonds; without one the command says so. */
 exists=false;
 linkr_bond_command(&conn,"bonds name orphan",out,sizeof out);assert(strstr(out,"ERR bonds not found"));
 exists=true;

 /* The slots fill up instead of silently overwriting somebody else. */
 {size_t peer=(size_t)label_find(0,"AA:BB:CC:DD:EE:01");
  char addr[18],name[16];
  label_drop(peer);
  for(unsigned i=1;i<=LABEL_SLOTS;i++){
   snprintf(addr,sizeof addr,"%u1:00:00:00:00:0%u",i%10,i%10);
   snprintf(name,sizeof name,"host-%u",i);
   assert(label_set(1,addr,name)==0);}
  assert(label_blob[1]==LABEL_SLOTS);
  linkr_bond_command(&conn,"bonds name overflow",out,sizeof out);
  assert(strstr(out,"ERR bonds name slots"));}

 /* Every slot, including the last, must hold a full label AND its NUL.
  * Update an earlier slot after filling the table to catch neighbour damage. */
 labels_reset();label_ready=true;
 {char addr[18], full[33]; memset(full,'x',32);full[32]='\0';
  for(unsigned i=0;i<LABEL_SLOTS;i++){
   snprintf(addr,sizeof addr,"AA:BB:CC:DD:EE:%02u",i);
   assert(label_set(1,addr,full)==0);
  }
  assert(label_set(1,"AA:BB:CC:DD:EE:00",full)==0);
  label_ready=false;labels_ensure();
  for(unsigned i=0;i<LABEL_SLOTS;i++){
   snprintf(addr,sizeof addr,"AA:BB:CC:DD:EE:%02u",i);
   assert(label_find(1,addr)==(int)i);
   assert(!strcmp((char*)label_slot(i)+2+LABEL_ADDR,full));
  }
  int before=saves;
  assert(label_set(1,"AA:BB:CC:DD:EE:00",full)==0&&saves==before);
  /* Maximum-sized response still fits the management stack buffer. */
  size_t used=(size_t)snprintf(out,sizeof out,"OK bonds names\n");
  for(unsigned i=0;i<LABEL_SLOTS;i++){
   snprintf(addr,sizeof addr,"AA:BB:CC:DD:EE:%02u",i);
   int n=name_line(out+used,sizeof out-used,1,addr,full);
   assert(n>0&&(size_t)n<sizeof out-used);used+=(size_t)n;
  }
 }
 /* Malformed NVS must be rejected before any strcmp or %s reads its slots. */
 {uint8_t good[LABEL_BLOB];memcpy(good,nvs,sizeof good);
  const size_t offsets[]={2,3+LABEL_ADDR-1,3+LABEL_ADDR,4+LABEL_ADDR+LABEL_MAX};
  for(size_t i=0;i<sizeof offsets/sizeof offsets[0];i++){
   memcpy(nvs,good,sizeof good);nvs[offsets[i]]=0xff;
   label_ready=false;labels_ensure();assert(label_blob[1]==0);
  }
  memcpy(nvs,good,sizeof good);
 }

 /* Removing the bond removes its label with it. */
 labels_reset();label_ready=true;
 linkr_bond_command(&conn,"bonds name doomed",out,sizeof out);assert(strstr(out,"OK bonds name"));
 linkr_bond_command(&conn,"bonds remove 0/AA:BB:CC:DD:EE:01",out,sizeof out);
 assert(strstr(out,"OK bonds queued"));
 remove_work.fn(NULL);
 assert(label_find(0,"AA:BB:CC:DD:EE:01")<0&&label_blob[1]==0);
 return 0;
}
