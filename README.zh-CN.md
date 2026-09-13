<p align="center">
  <img src="assets/linkr-bee-logo.svg" alt="Linkr Bee" width="520">
</p>

<p align="center">
  通过蓝牙或局域网把开发板的串口控制台搬到浏览器、手机和命令行。
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="docs/README.md">文档索引</a> ·
  <a href="docs/DEVELOPMENT.zh-CN.md">开发指南</a>
</p>

Linkr Bee 是一个 ESP32 小配件，把目标板的 UART 控制台通过蓝牙或局域网接到浏览器、
手机和命令行。

UART 常常是最后一个还能用的入口：新板启动、Bootloader 调试、网络起不来、系统恢复。
Linkr Bee 让这条控制台随时可连，不用常驻一根 USB 串口线、也不用把电脑守在目标设备旁。
它只转发目标 UART 的原始字节，不替代目标系统的 Shell。

## 能做什么

- 浏览器、手机 App、命令行三种终端，支持 ANSI 彩色与 256 色。
- 连接状态下改波特率、数据位、校验位、停止位和流控，默认 `115200,8,n,1,n`。
- 回车可按 raw、CR、LF、CRLF 发送，适配 Bootloader 和不同 Shell。
- 日志导出、WebDAV 上传，诊断面板可查固件、UART 缓冲、WiFi 和收发计数。
- 接入 2.4 GHz 网络后走局域网，不再需要蓝牙。
- 浏览器和手机 App 内置助手：检索串口日志、在你批准后向目标机执行命令，并以同样的
  方式修改桥接器自身的 UART、WiFi 和日志上传设置。它会记住每台目标设备的事实、统计
  本轮消耗的 token，并可以把整个排查过程导出成 Markdown 报告。

## 需要什么

```text
浏览器 / 电脑 / 手机 / Linkr
              │
        Bluetooth LE 或 WiFi
              │
      Linkr Bee 配件（ESP32-C3 / ESP32-C5）
              │
           3.3 V UART
              │
        SBC 或嵌入式设备
```

- 一台 Linkr Bee 配件：ESP32-C3 Super Mini、ESP32-C3 DevKitM、ESP32-C3 DevKitC
  或 ESP32-C5 DevKitC。
- 一台带 UART 控制台的目标设备，三根线：目标 TX 接配件 RX、目标 RX 接配件 TX、共地。
- 一台主机：桌面 Chrome/Chromium 用 Web 终端，或手机 App，或 macOS、Linux 上的
  Python / C 终端。

UART 一侧是 3.3 V 逻辑电平，不是 RS-232。自行设计硬件前先看
[硬件需求](docs/HARDWARE.md)。

## 快速开始

1. 刷固件。下载对应开发板的镜像（文件名见
   [开发指南](docs/DEVELOPMENT.zh-CN.md#github-actions-构建与刷写)），在 macOS 或
   Linux 上执行：

   ```sh
   python3 -m pip install esptool
   ./flash_firmware.sh
   ```

   脚本会自动找镜像和串口，有 `SHA256SUMS` 时校验，并保留已保存的设置。从源码构建见
   [开发指南](docs/DEVELOPMENT.zh-CN.md#构建)。

2. 接线：交叉连接 TX/RX，并连接 GND。ESP32-C3 Super Mini 上桥接 UART 为 GPIO20 (RX)
   和 GPIO21 (TX)，其他板子见[硬件需求](docs/HARDWARE.md#3-引脚分配)。

3. 给两侧上电。串口有数据时板载蓝灯闪烁。

4. 给当前主机授权：把 **GPIO1 接地**，连接并接受系统配对提示，完成后释放 GPIO1。
   这台主机会被记住。

5. 打开客户端。Web 终端：

   ```sh
   tools/serve_web.sh        # 然后打开 http://127.0.0.1:8765/
   ```

6. 把 UART 格式设成和目标一致，复位目标设备，看控制台输出。

## 使用说明

![Linkr Bee 桌面终端](assets/screenshots/linkr-bee-terminal-desktop.jpg)

- 顶部栏：连接状态、连接/重连、面板开关、主题、语言。
- 终端：点进去直接输入，工具条上有字号、全屏、自动滚动、复制、清屏。
- 控制面板：连接方式、UART 参数、回车格式、本地回显、分片大小、诊断、WiFi、WebDAV
  和日志操作。

UART 参数改完立即生效，目标机用非标准波特率启动时不用重新刷固件。终端还会在登录后和
窗口变化时同步目标机的窗口大小，全屏程序依赖这个。如果有输出但输入没反应，先确认目标
RX 接的是配件 TX，并且除非 CTS 和 RTS 都接了，否则关掉硬件流控。

<p align="center">
  <img src="assets/screenshots/linkr-bee-terminal-mobile.jpg" alt="Linkr Bee 手机终端" width="360">
</p>

手机上终端优先占用空间，次要设置收进抽屉。浏览器和手机 App 都有助手面板。浏览器里的
模型请求由页面直接发出，需要端点允许来自页面的请求——多数云端服务和本地模型服务都
支持。Anthropic 需要额外加一个请求头，配置方法见
[串口助手说明](mobile/README.md#built-in-serial-assistant-pi)。协议可选 OpenAI 兼容、
Anthropic 或 Gemini，对话按板子存在本机，刷新页面接着上次继续。

## 局域网模式

配件通过蓝牙配网接入 2.4 GHz 网络后，同一个终端可以改用它的 WebSocket 桥接访问。蓝牙
仍然是配置入口：UART 参数、WiFi、WebDAV 和诊断都走蓝牙，局域网模式只承载终端数据。

桥接需要 128 位访问令牌，首次启动生成，只能通过加密的蓝牙通道读到。Web 终端在蓝牙
连接期间自动获取，Python 和 C 终端会打印出来。令牌只控制访问、不做加密，`ws://` 流量
在同网段可见，请在可信网络里使用局域网模式。

## 配对与安全

- 蓝牙链路要求加密和绑定，每台新主机配对时把 **GPIO1 接地**授权一次。
- 最多记住八台主机，重启后保留，不会自动淘汰旧主机；要加第九台，先删掉不用的或恢复
  出厂。
- 配对使用 LE Secure Connections Just Works，没有中间人身份核验，只在能看见配件的
  场合授权。
- 恢复出厂会清除全部绑定和设置：ESP32-C3 启动时把 **GPIO0 接地**，ESP32-C5 用
  **GPIO28**，持续至少两秒。ESP32-C3 Super Mini 的 BOOT 按键不是 GPIO0，先确认板级
  接线。
- 各客户端步骤与恢复方法：[蓝牙配对与恢复](docs/BLE_PAIRING.md)。

## 常见问题

| 现象 | 检查 |
| --- | --- |
| 客户端里找不到设备 | 配件已上电、在范围内，且没有连到别的主机；同一时刻只服务一条蓝牙连接 |
| 浏览器连不上 | 用 Chrome 或 Chromium，且通过 HTTPS 或 `localhost` 打开；`tools/serve_web.sh` 用的就是 `localhost` |
| 没有输出 | 目标 TX 接了配件 RX、目标确实在输出、波特率一致 |
| 有输出但输入没反应 | 目标 RX 接了配件 TX；除非 CTS 和 RTS 都接了，否则关掉硬件流控 |
| 输出乱码 | UART 格式与目标一致：波特率、数据位、校验位、停止位 |
| 没有配对提示 | 连接之前就把 GPIO1 接地 |
| 局域网模式连不上 | 配件在 2.4 GHz 网络上、主机同网段、令牌与蓝牙通道读到的一致 |

## 已知限制

- 设备按 Linkr 管理服务匹配，`Linkr BLE UART-*` 后缀任意。
- Web Bluetooth 只有 Chrome 和 Chromium 支持，Safari 和 Firefox 没有该接口。
- WiFi 配网仅支持 2.4 GHz。
- iOS 模拟器不支持 BLE，需要真机。

## 文档

- [开发指南](docs/DEVELOPMENT.zh-CN.md)：构建、刷写、打包、Kconfig、协议
- [硬件需求](docs/HARDWARE.md)：引脚、电气要求、量产注意事项
- [蓝牙配对与恢复](docs/BLE_PAIRING.md)：授权、绑定与恢复
- [BLE 配件 API](docs/LINKR_BLE_API.zh-CN.md)：服务与分帧，面向集成方
- 手机客户端：[Android / iOS](mobile/README.md) · [HarmonyOS](harmonyos/README.md)
- [文档索引](docs/README.md) · [English README](README.md)

## 许可证

[LICENSE](LICENSE)
