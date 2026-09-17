# ESP32-C3 / ESP32-C5 硬件需求规格（Linkr Bee）

> 基于 C3/C5 board overlay、`src/main.c`、`prj.conf`、`Kconfig` 整理。
> C3 Super Mini 是当前硬件基线；C5 DevKitC 使用文中单独列出的差异配置。

## 1. 角色

ESP32 在系统中承担三重角色：

- **BLE 外设**：Nordic UART Service，与主机（手机/电脑/网关）双向串口
- **UART 桥**：把 BLE 数据透传到 SBC 串口，反之亦然
- **WiFi 上传节点**：连 WiFi 后把 SBC console 日志定期 PUT 到 WebDAV

## 2. SoC 选型

| 项 | 要求 |
|----|------|
| 芯片 | ESP32-C3，或 ESP32-C5 DevKitC 的 `hpcore` 目标 |
| BLE | BLE 5.0（内置） |
| WiFi | 当前固件使用 2.4GHz station 模式 |
| USB | 内置 USB Serial JTAG（烧录 + console，免外部芯片） |
| Flash | C3 ≥ 4MB；当前 C5 DevKitC 配置使用 8MB |
| PSRAM | 不需要 |

## 3. 引脚分配

ESP32-C3 Super Mini 基线：

| 功能 | GPIO | 方向 | 说明 |
|------|------|------|------|
| 桥接 UART0 RX | 20 | in | 接 SBC TX |
| 桥接 UART0 TX | 21 | out | 接 SBC RX |
| 活动 LED | 8 | out | 蓝色，收发活动闪烁（40ms 脉冲） |
| USB D- | 18 | — | 内置 USB Serial JTAG |
| USB D+ | 19 | — | 内置 USB Serial JTAG |
| RTS/CTS（可选） | 待定 | — | 流控，supermini 未接；量产可预留 2 个 GPIO |

> 桥接 UART 由 devicetree chosen 节点 `zephyr,linkr-ble-uart` 选定，量产可改 overlay 重映射。

ESP32-C5 DevKitC 差异：

| 功能 | GPIO | 方向 | 说明 |
|------|------|------|------|
| 桥接 UART1 RX | 12 | in | 接 SBC TX |
| 桥接 UART1 TX | 11 | out | 接 SBC RX |
| 恢复出厂 | 28 | in | BOOT，启动时接地保持 2 秒 |
| Console / 烧录 | 板载 USB | — | USB Serial/JTAG，不占用桥接 UART1 |

## 4. 桥接 UART 电气

- **电平**：3.3V TTL（非 RS232；需接 RS232 电平请外挂收发器）
- 默认 `115200,8,N,1,none`
- 支持波特率范围：300 ~ 3,000,000
- 数据位 5/6/7/8，校验 None/Odd/Even，停止位 1/2，流控 None/RTS-CTS
- 若 SBC 为 5V TTL，需加电平转换

## 5. LED 指示

- GPIO8，固件用 `gpio_pin_set_dt` 驱动
- 极性由 devicetree `gpios` 属性 flag 决定（supermini 为低电平点亮）
- 行为：
  - UART RX/TX 每次活动亮 40ms
  - loopback 失败时连闪 3 次（仅测试构建）

## 5.1 恢复出厂输入

- C3 使用 GPIO0；C5 DevKitC 使用 GPIO28（BOOT）。两者均启用内部上拉
- 在设备复位/上电时将对应输入短接至 GND，并保持 **2 秒**，固件才执行恢复出厂，避免启动瞬态误触发
- 固件在 Bluetooth 与 settings 初始化前擦除 `storage_partition`（NVS），清除 Bluetooth identity、WiFi/WebDAV 配置和上传 boot 计数器
- 不会擦除固件、`linkr-test-marker` 或 coredump 分区；BLE identity 会重新生成，原中心设备应删除旧设备记录后重新连接
- 该焊盘等同于物理恢复出厂权限，不应永久接地，也不应暴露给非授权人员

## 6. 控制台与烧录

- **Console**：USB Serial JTAG（`CONFIG_ESP_CONSOLE_USB_SERIAL_JTAG_ENABLED`），不占用桥接 UART
- **烧录**：esptool 通过同一 USB；C3 镜像地址为 `0x0`，C5 为 `0x2000`
- 量产可选预留 SWD/调试点（ESP32-C3 调试通过 USB JTAG）

## 7. RF / 天线

- BLE + WiFi 共享 2.4GHz 单天线，SoC 内置 coex
- 需 PCB 天线或 IPEX 座
- 推荐留 π 网络（匹配 + 滤波）
- 功放：遵循目标 ESP32-C3/C5 模组默认设计，无需外部 PA

## 8. 电源

| 项 | 要求 |
|----|------|
| 输入 | USB 5V 或 5V 主电源 |
| SoC 供电 | 3.3V LDO，纹波 < 50mV |
| 峰值电流 | ≥ 500mA（WiFi TX 瞬态） |
| LDO | 推荐 ≥ 600mA，如 AMS1117-3.3 / RT9013 |
| 去耦 | 每电源脚 0.1µF + 10µF |

## 9. 存储 / 分区

- C3 使用 4MB flash 与 `partitions_0x0_default` 分区表；C5 当前目标使用 8MB flash
- 含：bootloader + slot0 应用 + NVS（BLE identity、可选 WiFi 凭据和 WebDAV URL）+ coredump（可选）
- 诊断 marker 测试使用独立 4KiB `linkr-test-marker` 分区：C3 位于
  `0x3df000`，C5 位于 `0x7df000`；不写入相邻 coredump 分区
- 量产若需 OTA，改用 OTA 分区表

## 10. 测试点

| 测试点 | 用途 |
|--------|------|
| UART0 TX/RX（GPIO20/21） | 串口抓取 / 环回短接测试 |
| UART1 TX/RX（C5 GPIO11/12） | C5 串口抓取 / 环回短接测试 |
| GPIO8 LED | 状态观察 |
| GPIO0 + GND（C3） | 保持短接两秒后恢复出厂 |
| GPIO28/BOOT + GND（C5） | 保持短接两秒后恢复出厂 |
| USB（GPIO18/19） | console + 烧录 |
| EN/RST 按钮 | 复位（建议保留） |
| BOOT 按钮 | 进下载模式（建议保留） |

## 11. 兼容参考板

- **esp32c3_supermini**（主目标，已验证）
- esp32c3_devkitm / esp32c3_devkitc（overlay 已提供，引脚同上）
- esp32c5_devkitc/esp32c5/hpcore（UART1 GPIO11/12，USB Serial/JTAG console，GPIO28 恢复出厂）
- **esp32_devkitc/esp32/procpu**（ESP32-WROOM-32，Xtensa 双核，已验证）

### ESP32-WROOM-32 (Xtensa) 差异

| 项 | C3 Super Mini | WROOM-32 DevKitC |
|----|--------------|------------------|
| SoC | ESP32-C3 (RISC-V) | ESP32-D0WDQ6 (Xtensa 双核) |
| BLE | BLE 5.0 | BLE 4.2 |
| USB | 内置 USB Serial JTAG | 外置 CH340 USB-UART |
| 桥接 UART | UART0 (GPIO20/21) | UART2 (GPIO16 RX / GPIO17 TX) |
| Console | USB Serial JTAG | UART0 (GPIO1 TX / GPIO3 RX) via CH340 |
| LED | GPIO8 | GPIO2 |
| 配对按钮 | GPIO1 | GPIO4（默认不启用授权检查） |
| 恢复出厂 | GPIO0 (BOOT) | GPIO27（外接跳线接地） |
| DRAM | 单一 SRAM ~400KB | dram0 ~137KB + dram1 ~96KB |
| WebDAV | 默认开启 | **关闭**（DRAM 不足） |

**ESP32-WROOM-32 引脚分配：**

| 功能 | GPIO | 方向 | 说明 |
|------|------|------|------|
| 桥接 UART2 RX | 16 | in | 接 SBC TX（通过 CH340 → ttyUSB0） |
| 桥接 UART2 TX | 17 | out | 接 SBC RX（通过 CH340 → ttyUSB0） |
| Console UART0 TX | 1 | out | USB Type-C CH340（ttyUSB1） |
| Console UART0 RX | 3 | in | USB Type-C CH340（ttyUSB1） |
| 活动 LED | 2 | out | 蓝色，收发活动闪烁（40ms 脉冲） |
| 配对按钮 | 4 | in | 内部上拉，低有效；默认不启用授权检查 |
| 恢复出厂 | 27 | in | 外接跳线接地后上电或复位，保持至少 2 秒后释放 |

**配对与恢复说明：**

受当前硬件条件限制，WROOM-32 默认设置 `CONFIG_LINKR_BLE_BRIDGE_PAIRING_GPIO_AUTH=n`，附近主机无需按键即可申请配对。该选项只关闭物理授权检查；GATT 仍要求加密，绑定仍需保存，已有绑定重连仍恢复加密，未绑定完成或加密失败仍会断开。C3/C5 默认保留 GPIO 授权。

如果为 WROOM-32 外接了 GPIO4 配对按钮，可将该选项设为 `y` 恢复物理授权。恢复出厂使用独立的 GPIO27 跳线，会清除保存的设置与绑定；本板 GPIO0/BOOT 在复位时拉低会进入 ROM 下载模式，不能按 C3 的 GPIO0 恢复步骤操作。

**DRAM 约束与优化：**

ESP32-WROOM-32 的 dram1 仅 96KB，需要以下限制：

- `CONFIG_LINKR_BLE_BRIDGE_WEBDAV=n` — 关闭 WebDAV 节省 ~12KB noinit
- `CONFIG_LINKR_BLE_BRIDGE_UART_RX_BUFFER_SIZE=4096` — UART RX 从 16KB 降至 4KB
- BT ACL TX buffer 从 12 降至 4
- 各线程栈缩减（main=2048, workqueue=4096, WiFi connect=2048）
- HTTP server 单客户端，栈 2048

**功能差异：**

- WiFi station + BLE + WebSocket 桥接：完整支持
- WebDAV 上传：不可用（DRAM 不足）
- BLE 管理协议：完整支持
- Quick WiFi GATT：完整支持
- WebSocket 认证：完整支持

**已知限制：**

- BLE 4.2（非 5.0），MTU 协商最大 247
- 双 CH340 USB：ttyUSB0 = 桥接 UART2，ttyUSB1 = console UART0（端口可能因 USB 插拔顺序交换）
- WiFi/BLE 共存时功耗较高（power save 已禁用以降低延迟）

## 12. 固件对外接口（供硬件/集成联调）

- BLE 广播名：`Linkr BLE UART-3`
- NUS UUID：`6e400001` / `-02` / `-03` - `b5a3-f393-e0a9-e50e24dcca9e`
  - 服务：`6e400001-b5a3-f393-e0a9-e50e24dcca9e`
  - RX 写特征：`6e400002-b5a3-f393-e0a9-e50e24dcca9e`
  - TX 通知特征：`6e400003-b5a3-f393-e0a9-e50e24dcca9e`
- 正式配置提供 ATT/L2CAP MTU 247 与 LE Data Length 251；Reliable UART 的
  12 字节帧头和最多 232 字节 payload 可在协商成功时单包传输，较小 MTU
  仍自动分片并保留 20 字节兼容路径
- 控制命令：
  - `@u?` / `@u=baud,data,parity,stop,flow`（UART）
  - `@w=ssid,pass` / `@w off` / `@w?`（WiFi）
  - `@d=http://host/path/` / `@d off` / `@d?`（匿名 HTTP WebDAV）
  - `@h`（help）
- WiFi 默认启用但无线电关闭，`@w=` 后开启；凭据默认只在 RAM，只有启用 `CONFIG_LINKR_BLE_BRIDGE_PERSIST_CREDENTIALS=y` 才写入 NVS（要求 secure boot + flash encryption）
- GPIO1 为配对授权输入，内部上拉、低有效；通过按钮或跳线接 GND。Management 与两个 UART 服务要求加密 BLE；绑定持久化，已绑定主机重连无需拉低 GPIO1。GPIO1 不执行恢复出厂。详见 [蓝牙配对](BLE_PAIRING.md)。

## 13. 量产要点

1. WiFi/BLE 共用天线，RF 走线需 50Ω 阻抗控制
2. flash 容量 ≥ 4MB，需保留 NVS 存储区
3. 桥接 UART 引脚若与 SBC 距离 > 30cm，建议加 RS485 或电平缓冲
4. GPIO8 LED 量产可改其他空闲 GPIO（改 overlay `led0` alias 即可）
5. 预留 RTS/CTS 两脚以备流控升级
6. 预留 EN/RST、BOOT 与板级恢复出厂输入焊盘/按键，便于量产烧录和维修
7. USB D+/D- 走线差分 90Ω，ESD 保护器件靠近 USB 座

## 14. 构建 / 烧录命令（参考）

```sh
# 在含 hal_espressif 的 west workspace 内
west build -b esp32c3_supermini /path/to/linkr-bee
west flash --esp-device /dev/cu.usbmodemXXXX

west build -b esp32c5_devkitc/esp32c5/hpcore /path/to/linkr-bee
tools/flash_firmware.sh --image build/zephyr/zephyr.bin --chip esp32c5

# ESP32-WROOM-32 (Xtensa 双核)
west build -b esp32_devkitc/esp32/procpu /path/to/linkr-bee
west flash --runner esp32
```

前置条件：west manifest 含 `modules/hal_espressif`（提供 WiFi blobs）。

## 15. 变更记录

| 日期 | 版本 | 说明 |
|------|------|------|
| 2026-07-07 | v1.0 | 初版，对应已验证固件（含 WiFi/WebDAV） |
| 2026-09-16 | v1.1 | 新增 ESP32-WROOM-32 (Xtensa) 支持 |
