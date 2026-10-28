# 桥接队列丢包排查记录（固件侧）

> 最后更新: 2026-10-27
> 行号基准: 本文 `文件:行` 引用已按本 PR 的 HEAD 逐条重定位（以符号/字面量为锚点核对），不是写作当时的行号。
> 范围: 本文记录 **固件（板子）侧** 四处缓冲的丢包条件与影响（第 ④ 处见文末
> 2026-10-27 一节），供上游与后续排障参考。
> 客户端侧的三个问题（LAN 重连、上行限流、粘贴卡住）不在此列，见代码提交与本次 PR。

## 2026-10-04 三处缓冲的丢包条件与影响

### 结论

「队列满就丢」是本项目**从第一版就写死的取舍**，不是某块板子的硬件行为：
`git log -S"discarded + keep - written" -- src/ws_bridge.c` 指向
`e915983 feat: add WiFi and LAN WebSocket bridge`，并且有三处明文规定：

- `Kconfig:419` 帮助文本：*A client slower than the UART **drops oldest data
  instead of blocking** the bridge.*
- `docs/DEVELOPMENT.md:259` / `docs/DEVELOPMENT.zh-CN.md:207`：
  「每个客户端独立 TX 环形缓冲；**慢客户端丢弃最旧数据，不会拖慢桥**」
- `prj.conf` 出厂开关 `CONFIG_LINKR_BLE_BRIDGE_UART_RX_DROP_NO_CONN=y`，
  注释：*Disconnected BLE delivery is best-effort; **it cannot stall UART***。

硬件只决定**缓冲开多大**，不决定满了怎么办。

**板级配置现状（2026-10-11 逐项核对 `boards/esp32_devkitc_esp32_procpu.conf`）**：
这块板**没有把任何缓冲砍到 Kconfig 默认值以下**——两处同值，一处反而更大：

| 项 | Kconfig 默认 | 板级取值 |
|---|---|---|
| `WS_BRIDGE_CLIENT_BUFFER_SIZE` | 2048 | **2048**（`:242`） |
| `UART_RX_BUFFER_SIZE` | 16384 | **16384**（`:170`） |
| `WS_BRIDGE_MAX_CLIENTS` | 2 | **2**（`:234`） |
| `BLE_TO_UART_QUEUE_DEPTH` | 8 | **16**（`:179`，更大） |

> **历史注**：本文早先记的是「砍到 1024 B / `MAX_CLIENTS=1` / `UART_RX_BUFFER_SIZE=4096`」，
> 那是**压缩期**的取值（表格「压缩期为 …」括注保留，供对照当时的实测）；现配置已回到
> 并部分超过 Kconfig 默认。因此「RAM 紧的板子砍缓冲」不再适用于本板——**本板的丢包
> 是策略本身的性质（丢最旧 / 整条拒绝），不是缓冲被砍小造成的**。

### 链路与三处缓冲

```
SBC 控制台 ──115200 波特(8N1 ≈ 11520 B/s)──▶
  [① uart_rx_ring] ──每攒 232 B 或空闲 8 ms──▶ uart_forward_chunk()
                                               ├─▶ linkr_ws_feed() ─▶ [② 每客户端 tx_ring] ──256 B/次──▶ WS 客户端
                                               └─▶ BLE reliable / NUS ──▶ 蓝牙主机
  客户端 ──WS/BLE──▶ [③ ble_to_uart_queue] ──115200──▶ SBC
```

| # | 缓冲 | 默认大小 | ESP32-WROOM 板取值 | 满了怎么办 | 可观测性 |
|---|---|---|---|---|---|
| ① | `uart_rx_ring`（`src/main.c:188`） | 16384 B（≈1.42 s） | **16384 B**（≈1.42 s，与默认同值；压缩期为 4096 → 8192 B） | 计数丢弃（`src/main.c:466`） | `@i?` → `@info uart dropped=`（`src/main.c:1022`） |
| ② | 每客户端 `tx_ring`（`src/ws_bridge.c:73`） | 2048 B | **2048 B**（≈178 ms，与默认同值；压缩期为 1024 B/≈89 ms） | **丢最旧**（`src/ws_bridge.c:891-895`） | `@i?` → `@info ws … dropped=`（`src/ws_bridge.c:941`） |
| ③ | `ble_to_uart_queue`（`src/main.c:178`） | 深度 **8** × `BLE_TO_UART_MAX_LEN 244` = **1952 B**（≈170 ms） | 深度 **16** = **3904 B**（≈339 ms，**比默认大一倍**） | **整条拒绝** `-ENOMEM`（`src/main.c:1489` 预检 / `:1500` 单条入队） | **2026-10-06 起有计数器**：`@i?` → `@info queue dropped=`；`LOG_WRN` 也随日志级 1→2 恢复可见 |

### 丢包触发条件

**③ 上行（客户端 → SBC）**：`linkr_uart_write()` 在互斥锁内先算
`需要槽位 = ceil(字节数 / 244)`，空闲槽不够就 `err = -ENOMEM; goto out;`——
**整条写入一个字节都不入队**（不会发出半条命令）。因此：

1. 单次写入 **> 深度 × 244 → 必丢**，与队列空不空无关：Kconfig 默认 8 × 244 =
   **1952 B**，本板 16 × 244 = **3904 B**；
2. 单次没超但**当时已有积压**（排空整队需 ≈170 ms；本板 ≈339 ms）→ 同样整条丢。
   所以同一笔数据，队列空时过、有积压时丢，**结果是概率性的**。

**② 下行（SBC → 客户端）**：`linkr_ws_feed()` 每次喂入 ≤ `UART_RX_CHUNK`
（`Kconfig:34`，默认 232 B，范围 20–244），所以 `src/ws_bridge.c:886` 的
「单次超缓冲只留尾部」分支在本板**不可达**（232 < 2048）；实际丢的全部走
`src/ws_bridge.c:891` 的**丢最旧**。触发条件一句话：

```
WS 发送侧停顿 ≥ 2048 ÷ 11520 ≈ 178 ms，且 SBC 在满速打印
```

**① 上行采集侧**：只有 BLE 发送阻塞到把 16384 B 灌满才丢；本轮实测始终为 0，
且 `linkr_ws_feed()` 在 BLE 发送**之前**被调用、不看返回值（fire-and-forget），
所以 WS 慢不会回压到这里——这也是 `uart dropped=0` 而 `ws dropped` 大的原因。

### 什么时候会 / 不会发生

不会（稳态）：打字、回车、看几行输出（几 KB/s）；上行单次 < 1 KB；
下行只要发送侧不停顿 178 ms。

会（突发）：

- 下行：`cat` 大文件 / `dmesg` / `ls -R` / 开机日志 / 连续打印（`yes`、20000 Z）；
  粘贴大命令时回显与输出同时涌来；WiFi 省电、TCP 延迟 ACK 的瞬间。
- 上行：粘贴 > 队列容量的命令（本板 16 × 244 = 3904 B ≈ 3.8 KB；**默认深度 8 时
  实测 1.6 KB 起丢**，见下表）；上一条输出还在排空时再发；WS 与 BLE 共用同一队列。
- 连锁：单次 TCP 发送卡满 `WS_TX_TIMEOUT_MS 3000`（`src/ws_bridge.c:43`）
  → `WS send failed; closing`（`src/ws_bridge.c:567`）→ **直接踢掉该客户端**。

本质：11520 B/s 的硬带宽 + KB 级缓冲 = **只有百毫秒量级的容错窗口**
（本板 `tx_ring` 2048 B ≈ 178 ms；上传队列 3904 B ≈ 339 ms），
任何突发超过窗口就丢，与硬件好坏无关。稳态交互永远碰不到，所以平时感觉正常。

### 实测（LAN，2026-10-04）

> **配置口径**：本轮实测在**压缩期**——`tx_ring` 1024 B、`UART_RX_BUFFER_SIZE` 4096 B、
> 上传队列深度 8（均于 2026-10-06 调整）。故本表的丢包阈值反映的是当时更小的缓冲，
> **不等于上表现值**；现配置阈值按上表换算即可（`tx_ring` ×2 ⇒ 停顿窗口 89 → 178 ms，
> 上传队列 ×2 ⇒ 必丢阈值 1952 → 3904 B）。

| 场景 | 结果 |
|---|---|
| 上行 800 B（payload 自带 `printf \| wc -c`） | shell 报 800 → **不丢** |
| 上行 1600 / 3000 B | 丢 136 / 丢 1528；`--loopback-test` 1024、1200 PASS，**1600 起 FAIL** |
| 上行同尺寸走 **BLE** | 1600、2000 B 均 PASS（慢速灌不爆队列） |
| 下行 20000 Z（单轮） | `@info uart dropped=0`、`@info ws dropped=10864` |
| 下行多轮重复 | 丢量 **0 ~ 10864 随机**；每轮「客户端实收 + dropped」恒等 → 客户端不丢自己收到的 |
| 对照：最快读者（手写 WS 客户端） | 丢的量级与正式客户端相同 → **不是读端慢** |
| 对照：开/关 `--log-file` | 两组都既有 0 丢也有丢 → 日志开销与丢包无关 |

### 影响

1. **完全静默**：无错误码、无重传、无乱序标记。字节**只会少，不会错**
   （`ring_buf` 原样拷贝），收端无法察觉中间缺了一段。
2. **上行丢 = 整条命令凭空蒸发**（一个字节都不入队，收端无从察觉字节少了）。
   **但本板现已可观测**：`@i?` → `@info queue dropped=%u`（`src/main.c:1034`）
   有独立计数器；设备日志 `LOG_WRN("Dropping BLE->UART write; queue full")`
   （`src/main.c:1404`）在 `CONFIG_LOG_DEFAULT_LEVEL=2`（**WARN**，
   `boards/esp32_devkitc_esp32_procpu.conf:194`）下也会打印。
   > **历史注**：本文早先记的是「没有上行丢包计数器、日志级 1 只保留 ERROR ⇒ 连日志
   > 都没有」，那是**压缩期**的状态；计数器与日志级 1→2 均已加上，此条按现值更正。
3. **下行丢 = 终端输出中间缺块**（丢最旧，即**中段**而非尾巴）：
   `ls` 少几行、编译报错少几行、`wc -c` 数字偏小，而使用者以为看全了。
   这一项**可见**：`@i?` → `@info ws … dropped=N`。
4. **web 前端同样中招**：`web/app.js` 的 `state.ws.send(bytes)` 也是整包一发
   → 这是设备侧缺陷，与用哪个客户端无关。

### 应对

- **客户端（已做）**：LAN 写侧 8 KiB/s 令牌桶 + 1 KiB 分帧。1 KiB < 1952 B
  （本板 3904 B）有效突发容量，且 8192 B/s < 11520 B/s 排空速率 → **帧与帧之间队列必然排空**，
  上行理论上不再丢（待真机复验）。下行无客户端侧解法：读得再快也快不过设备的
  发送停顿，只能用 `@info ws dropped` 单独跟踪。

  RX 消费者必须持续排空环形缓冲区。高水位时停止消费者既不会停止 UART
  中断写入，也不会产生 RTS 背压，反而会使水位永远无法降到恢复阈值。
  原先的消费者水位门控已移除；没有硬件流控时，持续超出下游吞吐的输入
  仍可能丢失，扩大缓冲只能吸收突发。
- **固件（本文不改，留给上游）**：① 调大 `..._CLIENT_BUFFER_SIZE`
  （Kconfig 允许到 16384）；② 下行改成 TCP 背压
  （发送变慢就停止从 UART 取水）；③ 上行把 `K_NO_WAIT` 换成有限等待，让 TCP
  背压传导到慢客户端。三者任选其一即可消除对应丢点。

  > **本板 RAM 实测（2026-10-09，`west build` 内存报告）**：
  > `dram0` 138388 / 140452 B = **98.53%（只剩 2064 B）** ·
  > `dram1` 95040 / 98304 B = 96.68%（剩 3264 B） ·
  > `FLASH` 838624 / 4194048 B = 20.00%（很宽裕）。
  >
  > ① 的两个候选缓冲 —— `ble_to_uart_storage`（3936 B）与 `uart_rx_storage`
  > （16384 B）—— **都在 `.dram0.bss`**，所以「缩一个、涨另一个」是 DRAM0 内的零和。
  > 但 **`uart_rx_storage` 不能缩**：波特率是**自适应**的（`CONFIG_LINKR_BLE_BRIDGE_
  > UART_AUTOBAUD=y`，跟对端走、不要去动它），同一块缓冲的时间窗随档位变化——
  >
  > | 波特率 | 16384 B | 若缩到 8192 B |
  > |---|---|---|
  > | 115200 | 1421 ms | 711 ms |
  > | 921600 | 178 ms | 89 ms |
  > | 1500000 | 109 ms | **55 ms** |
  >
  > 自适应到高波特率时缓冲等于变相缩小，再减半会直接开始丢数据。
  > 结论：**本板不靠改缓冲大小换吞吐**；要提速走 BLE 侧（写请求 → 免回复写），
  > 而且客户端写入速率必须始终 ≤ 最慢档的排空速率 11520 B/s。

## 2026-10-27 第四处缓冲：BLE 输出队列（ESP32-C5 上会踢掉连接）

上游多客户端共享（`feat(bridge): share serial output across clients with
single-writer control`）新增了 `src/ble_peers.c`：每个 BLE 连接一个
`k_msgq` 输出队列，`uart_to_ble_thread` 通过 `linkr_ble_feed()` 把 UART 数据
按 232 B 一片入队，再由每连接一个 `ble_peer_tx` 线程发出去。这成为**第 ④ 处
缓冲**，链路如下：

```
SBC 控制台 ──115200──▶ [① uart_rx_ring] ──232 B─▶ uart_forward_chunk()
                                                    └─▶ linkr_ble_feed()
                                                          └─▶ [④ 每 peer msgq]
                                                                └─▶ linkr_nus_send() 232 B/包
```

| # | 缓冲 | 位置 | 大小 | 旧行为（≤ `fa407be`） | 现行为 |
|---|---|---|---|---|---|
| ④ | 每 peer `msgq` | `src/ble_peers.c`（`QUEUE_DEPTH` × `PAYLOAD_SIZE`） | Kconfig 默认 8 × 232 B = 1856 B；C5 板曾配成 **4 = 928 B** | 队列满 → 立刻 `bt_conn_disconnect(REMOTE_USER_TERM)`，**无任何日志** | 计数丢弃 + 限速 `LOG_WRN`，连接保留；`@i?` → `@info peer dropped=` |

### 现象

ESP32-C5 DevKitC 上，SBC（`radxa-cubie-a5e`）登录后只要输出稍微多一点的命令就会
掉线。最小复现是 `ls -l /usr/bin | head -40`（约 3 KB、16 个 notification）：命令
发出后链路断开，终端报废。固件日志只有 `BLE disconnected: 0x13`，**没有任何**警告
说明是谁、为什么断的。

`@i?` 当时没有任何 UART→BLE 方向的丢弃计数，因此第 ④ 处丢包/断连完全不可观测。
`docs/` 的其余排障记录（`uart dropped`、`ws dropped`、`queue dropped`）也都看不到它。

### 历史实测（C5 DevKitC，2026-10-27，扩大缓冲的实验镜像）

下表来自缓冲扩大阶段（BLE 队列 8 个分片）的实验镜像，不是最终
`6301066` 配置的验收结果。该阶段构建使用过本地修改的 Zephyr 内存边界；
最后一次板级修正已恢复官方 ROM 保留区，并缩减缓冲以适配未修改的 Zephyr v4.4.1。
因此下表用于保留问题定位经过，不能据此承诺最终镜像的零丢包或吞吐。

| 场景 | 修复前 | 修复后 |
|---|---|---|
| `ls -l /usr/bin \| head -40`（约 3 KB） | **断连** | 保持连接，`peer dropped=0` |
| `ls -l /usr/bin \| head -400`（约 24 KB） | **断连** | 保持连接，收到 18361 B，丢 6960 B |
| 20 条命令 / 72 s（约 56 KB 输出，混合 `ls`、`dmesg`、`/proc/interrupts`） | 断连 | 保持连接，收到 56482 B，丢 13828 B |

上述实验镜像 BLE 下行有效吞吐约 **780 B/s**（72 s 内送出 56 KB），仍明显低于 UART 的
11520 B/s，因此突发时丢字节是预期结果——与 ① ② ③ 的取舍一致：**丢字节，不踢人**。

### 改动

- `src/ble_peers.c`：队列满时 `atomic_add(&peer_tx_dropped, len)` 并继续，
  不再 `closing = true` + `bt_conn_disconnect()`；丢弃日志按 1 s 限速，避免日志
  自身在丢包时刷屏。`peer_tx` 发送失败（`err && err != -ENOTCONN`）仍然断开，
  但先打一条 `LOG_WRN`，不再静默。
- `src/main.c`：`@i?` 增加 `@info peer dropped=`；TUI（`DESIGNED_GROUPS` 之外的
  组会追加显示）与 web 解析器都按组名取行，未知组被容忍，因此不需要客户端改动。
- 最终 `boards/esp32c5_devkitc_esp32c5_hpcore.conf` 的 BLE 队列仍为 4 个分片
  （928 B 有效载荷），WS 每客户端缓冲仍为 1024 B、客户端数为 1；
  UART RX 和 WebDAV 日志缓冲由 2048 B 缩为 1024 B。此前 8 分片 / 2048 B WS
  的实验配置及其 SRAM 百分比不代表最终镜像。
- `6301066` 的记录是：在未修改的 Zephyr v4.4.1 上链接占用
  374816 / 378384 B，实机 WiFi 关联和 DHCP 成功。本次未补做最终缓冲配置下的
  BLE/WS 突发吞吐复测；合入后的性能验收应使用该配置重新运行上表命令。

> 取舍说明：上游原注释是 *Close slow peers instead of silently corrupting their
> stream*。实测表明在 C5 上这条策略把「一次 `ls` 输出」直接变成「终端断开」，
> 而断连后用户必须重新配对/重连才能继续，代价远大于丢几行输出。丢包策略与
> ① ② 以及 `UART_RX_DROP_NO_CONN` 的既有取舍保持一致。

### LAN（WebSocket）侧同一天的历史实验

同一台 C5 通过 LAN 连接时，第 ② 处（每客户端 `tx_ring`）暴露出同类问题，但性质
不同——它**不会断连**，只是把输出丢光：

| 场景（LAN，单客户端） | 1024 B（改前） | 说明 |
|---|---|---|
| `echo lan-claim-ok`（约 120 B） | 完整收到 | 小突发无压力 |
| `ls -l /usr/bin \| head -200`（约 24 KB） | 收到 **37 B**，`@info ws … tx=1316 rx=47 dropped=10333` | ring 一直被覆盖，客户端几乎什么都看不到 |

实验中曾把 `WS_BRIDGE_CLIENT_BUFFER_SIZE` 从 1024 改为默认的 2048；
最终因 ROM 保留区约束回到 1024。下文的双客户端 / SRAM 测量同样属于实验配置，
不是最终镜像的容量或稳定性保证。

`WS_BRIDGE_MAX_CLIENTS` 停在 1，原因是第二个客户端根本连不上，不是稳定性问题：
历史记录声称配成 2 的镜像连续跑了 25 分钟，但所列 uptime
`1473624 → 1503959 ms` 只覆盖约 30 秒，不能单独证明整个 25 分钟窗口。该片段内
`Booting Zephyr`／`Reset cause` 计数为 0；记录中的单客户端连接、串行重连 ×3、
`@control claim` + `echo` 往返全部通过。而这个槽位要多占一个 2048 B ring **和**一个
3072 B 线程栈，SRAM 从 96.34% 涨到 98.86%（只剩 4.4 KB）。

即使配成 2，第二个 `ws://` 连接也**在 TCP 层就被拒**（客户端 `ConnectionRefused`，
固件里连 `linkr_ws_setup()` 都没进，没有 `[1] WS client connected`，也没有
`Rejecting WS client; all %d slots busy`）。Zephyr 的
`subsys/net/lib/http/http_server_core.c:653` 在 `service->data->num_clients >=
concurrent` 时把监听 socket 的 `events` 置 0，等于停止 accept；`@s off` + `@s on`
也救不回来（该路径只在服务首次初始化时把 `num_clients` 清零）。把
`NET_MAX_CONTEXTS` 提到 8、`NET_MAX_CONN` 提到 6 均无效，排除 socket 耗尽。

想开 `CONFIG_NET_LOG` + `NET_HTTP_SERVER_LOG_LEVEL_DBG` 抓 accept 细节时，SRAM 走到
98.92%，固件**卡在启动**：串口停在 `net80211 rom version`，进不到
`*** Booting Zephyr OS ***`，`libc heap size` 从 13 kB 掉到 4 kB。C5 的内存余量已经
不够再做这层诊断。因此最终保留 `MAX_CLIENTS=1`，避免额外的客户端缓冲和线程栈；
第二个客户端的根因仍需在独立、可复现的配置上继续核实。

WebDAV、握手与令牌轮换实测正常（见对话记录）：错误令牌与 3 s 内不发令牌都会被断开、
`@s token` 轮换后旧令牌失效、`@s token off` 后握手变成 `auth=none`、
`@d=http://<host>/` 能在对端收到 `log-<boot>-<seq>-<uptime>.txt`（201）。

同日还修掉一个与缓冲无关的会话缺陷：`@s?` / `@d?` 没有被算进 `src/main.c` 的只读
命令表，因此一次**查询**就会通过 `linkr_session_command_begin()` 抢占串口写权，把
另一条通道（例如 LAN 客户端）锁成 `mode=readonly` 并拒绝其写入（`ERR control=readonly`）。
只读表已补上 `d?`/`webdav?` 与 `s?`/`socket?`。
