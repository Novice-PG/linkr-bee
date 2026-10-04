# 桥接队列丢包排查记录（固件侧）

> 最后更新: 2026-10-04
> 范围: 本文只记录 **固件（板子）侧** 三处缓冲的丢包条件与影响，供上游与后续排障参考。
> 客户端侧的三个问题（LAN 重连、上行限流、粘贴卡住）不在此列，见代码提交与本次 PR。

## 2026-10-04 三处缓冲的丢包条件与影响

### 结论

「队列满就丢」是本项目**从第一版就写死的取舍**，不是某块板子的硬件行为：
`git log -S"discarded + keep - written" -- src/ws_bridge.c` 指向
`e915983 feat: add WiFi and LAN WebSocket bridge`，并且有三处明文规定：

- `Kconfig:369` 帮助文本：*A client slower than the UART **drops oldest data
  instead of blocking** the bridge.*
- `docs/DEVELOPMENT.md:259` / `docs/DEVELOPMENT.zh-CN.md:208`：
  「每个客户端独立 TX 环形缓冲；**慢客户端丢弃最旧数据，不会拖慢桥**」
- `prj.conf` 出厂开关 `CONFIG_LINKR_BLE_BRIDGE_UART_RX_DROP_NO_CONN=y`，
  注释：*Disconnected BLE delivery is best-effort; **it cannot stall UART***。

硬件只决定**缓冲开多大**，不决定满了怎么办。RAM 紧的板子只是让既定策略更容易触发：
板级配置 `boards/esp32_devkitc_esp32_procpu.conf` 反而把下行缓冲**砍到 1024 B**
（Kconfig 默认 2048）——`CONFIG_LINKR_BLE_BRIDGE_WS_BRIDGE_CLIENT_BUFFER_SIZE=1024`、
`MAX_CLIENTS=1`、`UART_RX_BUFFER_SIZE=4096`，注释写明原因是 Xtensa DRAM1 noinit
预算紧张（*needs tighter buffer limits*）。

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
| ① | `uart_rx_ring`（`src/main.c:159`） | 16384 B（≈1.4 s） | **4096 B**（≈355 ms） | 计数丢弃（`src/main.c:436`） | `@i?` → `@info uart dropped=` |
| ② | 每客户端 `tx_ring`（`src/ws_bridge.c:73-74`） | 2048 B | **1024 B**（≈89 ms） | **丢最旧**（`src/ws_bridge.c:882-888`） | `@i?` → `@info ws … dropped=`（`src/ws_bridge.c:920`） |
| ③ | `ble_to_uart_queue`（`src/main.c:155`） | 深度 **8** × `BLE_TO_UART_MAX_LEN 244` = **1952 B**（≈170 ms） | 同左（板级未改） | **整条拒绝** `-ENOMEM`（`src/main.c:1389-1392`） | **无计数器**；只打 `LOG_WRN`，而板级把 WARN 关了 |

### 丢包触发条件

**③ 上行（客户端 → SBC）**：`linkr_uart_write()` 在互斥锁内先算
`需要槽位 = ceil(字节数 / 244)`，空闲槽不够就 `err = -ENOMEM; goto out;`——
**整条写入一个字节都不入队**（不会发出半条命令）。因此：

1. 单次写入 **> 8 × 244 = 1952 B → 必丢**，与队列空不空无关；
2. 单次 ≤1952 B 但**当时已有积压**（排空整队需 ≈170 ms）→ 同样整条丢。
   所以同样 1.4 KB，队列空时过、有积压时丢，**结果是概率性的**。

**② 下行（SBC → 客户端）**：`linkr_ws_feed()` 每次喂入 ≤ `UART_RX_CHUNK`
（`Kconfig:34`，默认 232 B，范围 20–244），所以 `src/ws_bridge.c:877` 的
「单次超缓冲只留尾部」分支在本板**不可达**（232 < 1024）；实际丢的全部走
`src/ws_bridge.c:882` 的**丢最旧**。触发条件一句话：

```
WS 发送侧停顿 ≥ 1024 ÷ 11520 ≈ 89 ms，且 SBC 在满速打印
```

**① 上行采集侧**：只有 BLE 发送阻塞到把 4096 B 灌满才丢；本轮实测始终为 0，
且 `linkr_ws_feed()` 在 BLE 发送**之前**被调用、不看返回值（fire-and-forget），
所以 WS 慢不会回压到这里——这也是 `uart dropped=0` 而 `ws dropped` 大的原因。

### 什么时候会 / 不会发生

不会（稳态）：打字、回车、看几行输出（几 KB/s）；上行单次 < 1 KB；
下行只要发送侧不停顿 89 ms。

会（突发）：

- 下行：`cat` 大文件 / `dmesg` / `ls -R` / 开机日志 / 连续打印（`yes`、20000 Z）；
  粘贴大命令时回显与输出同时涌来；WiFi 省电、TCP 延迟 ACK 的瞬间。
- 上行：粘贴 > ~1.4 KB 的命令；上一条输出还在排空时再发；WS 与 BLE 共用同一队列。
- 连锁：单次 TCP 发送卡满 `WS_TX_TIMEOUT_MS 3000`（`src/ws_bridge.c:43`）
  → `WS send failed; closing`（`src/ws_bridge.c:566`）→ **直接踢掉该客户端**。

本质：11520 B/s 的硬带宽 + 1 KB 级缓冲 = **只有 89 ms 的容错窗口**，
任何突发超过窗口就丢，与硬件好坏无关。稳态交互永远碰不到，所以平时感觉正常。

### 实测（LAN，2026-10-04）

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
2. **上行丢 = 整条命令凭空蒸发，且在 ESP32-WROOM 板上不可观测**：
   `@i?` 没有上行丢包计数器，设备日志 `LOG_WRN("Dropping BLE->UART write; queue full")`
   （`src/main.c:1304`）是 WARN 级，而 `boards/esp32_devkitc_esp32_procpu.conf:30`
   `CONFIG_LOG_DEFAULT_LEVEL=1` 只保留 ERROR → 连日志都没有。
3. **下行丢 = 终端输出中间缺块**（丢最旧，即**中段**而非尾巴）：
   `ls` 少几行、编译报错少几行、`wc -c` 数字偏小，而使用者以为看全了。
   这一项**可见**：`@i?` → `@info ws … dropped=N`。
4. **web 前端同样中招**：`web/app.js` 的 `state.ws.send(bytes)` 也是整包一发
   → 这是设备侧缺陷，与用哪个客户端无关。

### 应对

- **客户端（已做）**：LAN 写侧 8 KiB/s 令牌桶 + 1 KiB 分帧。1 KiB < 1952 B
  有效突发容量，且 8192 B/s < 11520 B/s 排空速率 → **帧与帧之间队列必然排空**，
  上行理论上不再丢（待真机复验）。下行无客户端侧解法：读得再快也快不过设备的
  发送停顿，只能用 `@info ws dropped` 单独跟踪。
- **固件（本文不改，留给上游）**：① 调大 `..._CLIENT_BUFFER_SIZE`
  （Kconfig 允许到 16384，本板受 DRAM 限制需实测上限）；② 下行改成 TCP 背压
  （发送变慢就停止从 UART 取水）；③ 上行把 `K_NO_WAIT` 换成有限等待，让 TCP
  背压传导到慢客户端。三者任选其一即可消除对应丢点。
