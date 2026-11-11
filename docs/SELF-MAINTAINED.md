# ESP32-WROOM-32 自维护固件说明

> 状态：**Novice-PG fork 自维护**，不属于 radxa 官方固件，也不进入上游 PR。
> 适用板型：仅 `esp32_devkitc/esp32/procpu`（ESP32-WROOM-32 DevKitC）。
> 固件版本：`1.2.0`（与 radxa 上游对齐，见下文“如何分辨”）。
> 分支：`fork/self-maintained`。

## 为什么单独维护

radxa 上游 1.2.0 把 WROOM-32 的 UART 接收环压到 4,096 B、BLE 队列压到 4、
WS 缓冲压到 1,024 B 以节省 SRAM。本分支在 1.2.0 代码基础上把这三项恢复到
上游缩减前的大小，版本号与上游一致，烧录页可以把自维护包和官方 1.2.0 放在
同一条发布线上。其余配置跟随上游。诊断日志、吞吐余量、内部堆余量都不属于
这个标准，因此不加大。

其他板型（C3 / C5 / X1.0）不在这个分支的改动范围内，仍然跟随上游配置。

## 与官方 WROOM 构建的差别

| 配置 | 上游 1.2.0 配置 | 本分支 | DRAM0 变化 | 用户能感知到什么 |
|---|---|---:|---:|---|
| `LINKR_BLE_BRIDGE_PEER_TX_QUEUE_DEPTH` | 4 | **8** | +1,920 B | BLE 终端每个 peer 的突发窗口约 0.7 s → 1.4 s；手机 / Windows 主机短暂停顿时少丢一段输出 |
| `LINKR_BLE_BRIDGE_WS_BRIDGE_CLIENT_BUFFER_SIZE` | 1,024 | **2,048** | +2,048 B | 浏览器终端每个客户端的 TX 环约 89 ms → 178 ms；渲染卡顿时少丢最旧的输出 |
| `LINKR_BLE_BRIDGE_UART_RX_BUFFER_SIZE` | 4,096 | **8,192** | +4,096 B | 调度或 BLE/WS 短暂阻塞时，UART 环能多吸收约 355 ms 的串口输入，少丢一段正在传输的数据 |

合计占用 **8,064 B**。2026-11-08 的 CI（Zephyr 4.5，提交 `7a13f09`）链接结果为
`dram0_0_seg` 已用 **139,644 / 140,452 B**，**剩余 808 B**；`dram1_0_seg`
仍为 94,424 / 95,232 B（剩余 808 B），没有被这次改动占用。符号大小与预算一致：
`peer_storage` 3,840 B、`ws_tx_bufs` 4,096 B。

### 1. BLE 输出队列 4 → 8

`src/ble_peers.c` 为每个 BLE 连接维护一个有界 `k_msgq`，`linkr_ble_feed()`
以 `K_NO_WAIT` 入队；队列满时丢最旧的一片并累加 `peer dropped`，不会断开连接。
BLE 是整条链路最慢的一段（实测可靠模式约 1.0–1.4 KiB/s），所以它也是突发时
最先丢字节的地方。深度 4 → 8 把“一次 `ls` / 一段 agent 输出”的容忍窗口翻倍；
持续输出快于 BLE 时仍然会丢，这是既有策略，不靠加缓冲改变。

### 2. WebSocket 每客户端 TX 环 1,024 → 2,048

`src/ws_bridge.c` 的 `ws_tx_bufs[]` 是丢最旧字节的环形缓冲。1,024 B 是压缩期
的取值，只有 Kconfig 默认 2,048 B 的一半；改回默认后，浏览器标签页在渲染、
后台节流或 TCP 窗口暂时关闭时能多保留约 89 ms 的串口输出。它不会让一个
休眠数秒的标签页“不丢”，只是把丢的位置推迟。

### 3. UART 接收环 4,096 → 8,192

`uart_to_ble_thread()` 从 UART 环取数据后按 BLE/WS 的节奏 fan-out。上游 1.2.0
把环减半以省 SRAM；恢复到 8,192 B 后，调度停顿或 BLE/WS 短暂阻塞时能多吸收
约 355 ms 的串口输入，避免把正在传输的一段输出从中间截断。它不改变持续丢包
的上限——输出长期快于 BLE 仍然会丢——只是把突发容忍窗口拉回上游缩减前。

### 补充：`ZVFS_POLL_MAX` 已由上游修复

Zephyr 4.5 的 HTTP 服务器固定这样轮询（`http_server_core.c`）：

```c
#define HTTP_SERVER_SOCK_COUNT \
    (1 + HTTP_SERVER_SOCKETS_PER_SERVICE * HTTP_SERVER_MAX_SERVICES + \
     HTTP_SERVER_MAX_CLIENTS + HTTP3_SERVER_MAX_STREAMS)   /* = 4 */
...
zsock_poll(ctx->fds, ARRAY_SIZE(ctx->fds), -1);
```

而 `zvfs_poll_internal()` 的事件表只有 `CONFIG_ZVFS_POLL_MAX` 格，每个带
`POLLIN` 的 fd 占一格：

- stop eventfd：1
- 监听 socket：1
- 每个**尚未完成 WebSocket 升级**的客户端：1

本构建 `HTTP_SERVER_MAX_CLIENTS=2`，两个客户端同时握手就需要 4 格，而默认只有
3 格。第 4 个 `POLL_PREPARE` 返回 `-ENOMEM`（errno 12，不是堆内存不足），
`http_server_run()` 走 `closing:`，关闭监听器和所有未升级连接，然后
`sleep(1000 ms)` 再重新初始化。已经升级完成的 WS 会话在
`http_server_release_client()` 时已从 poll 表移除，不会被这次重启关掉；受影响的
是**握手中的连接**和这 1 秒内的新连接。

默认 `CONFIG_NET_HTTP_SERVER_LOG_LEVEL=0`，所以 `poll failed` 和
`Re-starting server` 都不会出现在日志里，表现成“偶尔连不上，再试就好”。
`ZVFS_POLL_MAX=5` 给 eventfd + 监听 + 2 客户端留 1 格余量。

这不是缓冲放大，但同样属于“花配置空间换体验”，而且 C3 / C5 也有同样的
风险。上游已在 `7c4922ac` 用相同的顶层 `Kconfig` 默认值（按
`LINKR_BLE_BRIDGE_WS_BRIDGE` 生效）修复，并加了编译期断言；本分支直接使用
上游定义，不再维护重复条目。

## 跟随上游

`fork/self-maintained` 始终以 radxa `main` 为基础：上游发版或调整配置后，先
`git merge upstream/main` 吸收，再只保留本文列出的三项缓冲改动。上游已经自己
修好的项（例如 `ZVFS_POLL_MAX`）直接删掉 fork 的重复实现，不另立版本号。
每次对齐后重新跑 `Build and test`，确认产物里的 `firmware.config` 仍包含：

```text
CONFIG_LINKR_BLE_BRIDGE_UART_RX_BUFFER_SIZE=8192
CONFIG_LINKR_BLE_BRIDGE_PEER_TX_QUEUE_DEPTH=8
CONFIG_LINKR_BLE_BRIDGE_WS_BRIDGE_CLIENT_BUFFER_SIZE=2048
CONFIG_ZVFS_POLL_MAX=5
```

## 明确不加大

| 项 | 原因 |
|---|---|
| `LOG_BUFFER_SIZE` 1,024 → 8,192 | 只影响诊断日志，不影响用户看到的串口内容 |
| `HEAP_MEM_POOL_SIZE` 52,000 → 更大 | 内部余量，没有直接体验收益 |
| WiFi / net_buf / net_pkt 池 | 吞吐和抗丢包余量，不是用户可见功能的改变 |
| 线程栈（system workqueue 等） | 在 DRAM1，且剩余仅 808 B；属于内部安全余量 |
| 第三个 WS 客户端 | 需要额外 3,072 B DRAM1 线程栈，当前放不下 |

## 如何分辨这是自维护固件

- 版本号仍然是 `1.2.0`，与上游一致，便于客户端和发布流程继续工作。
- 烧录页 <https://novice-pg.github.io/linkr-bee/flash.html> 有两个 WROOM-32
  条目：官方 `esp32-wroom-32`，以及带 `Novice-PG 自维护` 标注的
  `esp32-wroom-32-self`，选择后状态栏会说明差异。
- `firmware.json` 里的 `git_commit` 指向本分支的提交，而不是 radxa 的 tag。
- 官方固件仍然从 <https://github.com/radxa/linkr-bee/releases> 获取；
  官方 WROOM 构建不带上述三项缓冲改动。

## 更新烧录页

烧录页（`pages.yml`，从 `main` 的 `web/flash.html` 发布）从
`fork/self-maintained` 的**最近一次成功 Build and test 运行**里取 WROOM
产物，因此缓冲区改动不会进入官方 release 资产，也不会覆盖官方 WROOM ZIP。
旧的 `fork/web-flasher` 分支已不再参与 Pages 部署，保留仅作历史参考。

1. 推送本分支。
2. 在 Actions 里对 `fork/self-maintained` 手动运行一次 `Build and test`，
   等 WROOM job 成功。
3. 烧录页会在下一次部署时自动出现/刷新 `esp32-wroom-32-self` 条目；也可以
   手动运行 `Deploy the web terminal and the firmware flasher to GitHub Pages`。

Actions artifact 默认保留 30 天；过期后重新运行一次构建即可。页面上的
`firmware.json.git_commit` 是固件对应的提交，不一定是分支文档的最新提交。

## 构建与验证

CI 使用 Zephyr v4.5.0、Zephyr SDK 1.0.1（与上游构建矩阵一致）：

```sh
west build -b esp32_devkitc/esp32/procpu .
```

提交前检查：

1. 链接后 `dram0_0_seg` 空闲应为 **808 B**；`peer_storage` 应为 3,840 B，
   `ws_tx_bufs` 应为 4,096 B。2026-11-08 已在 CI 构建（提交 `7a13f09`）中核对。
2. UART 环为 8,192 B、BLE peer 队列为 8、WS TX 环为 2,048 B；任一值被上游
   合并覆盖都应先修正再发布。
3. BLE 连接下跑一段突发输出，对比 `@i?` 的 `peer dropped`。
4. 同时打开两个 WS 连接（或一个握手 + 一个预连接），确认 HTTP 服务不再整段
   重启；把 `CONFIG_NET_HTTP_SERVER_LOG_LEVEL` 临时设为 3 可以看到
   `poll failed (-12)` 在修复前出现、修复后消失。

## English summary

This branch carries Novice-PG's self-maintained ESP32-WROOM-32 firmware. It
tracks radxa's `1.2.0` version and is based on upstream `main`, but it differs
from the upstream 1.2.0 configuration in three buffers: the UART RX ring is
8,192 B instead of 4,096, the BLE peer TX queue is 8 chunks instead of 4, and
the per-client WebSocket TX ring is 2,048 B instead of 1,024 B. `ZVFS_POLL_MAX` is 5, matching upstream
1.2.0, which adopted the same default and added a build-time assertion; the
fork relies on that upstream definition instead of carrying its own. Total
added DRAM0 over the upstream 1.2.0 configuration is 8,064 B; the CI link
(Zephyr 4.5, commit `7a13f09`) uses 139,644 of 140,452 B and leaves exactly
808 B free.
The log buffer, the system heap, WiFi/network pools and thread stacks are
deliberately not enlarged: their benefit is diagnostics, throughput or
internal headroom, not a better console.
Other boards are unchanged and still follow upstream. Official firmware:
<https://github.com/radxa/linkr-bee/releases>.
