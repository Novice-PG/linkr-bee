# Linkr Bee 终端教程

> [项目首页](../README.zh-CN.md) · [English](TUTORIAL.md) ·
> [文档索引](README.md) ·
> [Rust 终端参考](../tools/linkr-cli/README.md)

本教程手把手带你用 `linkr` 终端客户端：怎么安装、怎么打开 TUI、
以及怎么全程用键盘操作每一个界面。默认你已经刷好并接好了 Linkr Bee 配件 ——
刷写、接线、配对五步见[项目首页](../README.zh-CN.md#快速开始)。

所有界面都有中英两版；[第 12 步](#12-切换界面语言)讲怎么切换。

## 1. 你会得到什么

`linkr` 是一个二进制文件，两种用法：

- **TUI** —— OpenCode 风格的界面：侧栏、五个可切换视图、命令面板和模态对话框，
  全部键盘操作。
- **普通 CLI 模式** —— 与 Python 客户端相同的参数，给脚本和 CI 用：
  `linkr --scan`、`linkr --query-info --no-terminal`、`linkr --wifi …`。

```text
┌─────────────┬──────────────────────────────────────────────┐
│ 连接        │                                              │
│ ● 已连接    │              当前视图                        │
│ 传输方式    │         （F2 终端 / F3 诊断 /                │
│ 设备        │          F4 网络 / F5 助手 /                 │
│             │          F6 文件传输）                       │
│             │                                              │
│ 快捷发送    │                                              │
│             ├──────────────────────────────────────────────┤
│ 监控        │ [焦点] 视图 · 详情   Ctrl+P · F1 · Ctrl+Q    │
└─────────────┴──────────────────────────────────────────────┘
```

## 2. 安装

### 从源码构建

```sh
tools/build_terminal.sh          # 先跑 fmt + clippy + 测试，再做 release 构建
dist/linkr-terminal-linux-aarch64/linkr --version
```

脚本会先跑完整校验套件，任何一项不通过就拒绝打包。
`--target <三元组>` 交叉构建，`--no-verify` 跳过校验（仅在反复迭代时使用）。

### 直接拿现成的二进制

推送 `v*` 标签后，CI 会把这次构建的全部产物挂到仓库的 **Releases** 页——那里的
文件长期有效（workflow 上的 artifact 只保留 30 天）：

```
https://github.com/radxa/linkr-bee/releases/latest
```

Linux 是 `linkr-terminal-<slug>.tar.gz`，Windows 是
`linkr-terminal-<slug>.zip`，解开后的目录名都与 `tools/build_terminal.sh` 本地
打包**完全一致**，所以校验命令也和本地构建一模一样；Release 顶层另有一份覆盖
全部文件的 `SHA256SUMS`：

```sh
tar xzf linkr-terminal-linux-aarch64.tar.gz
cd linkr-terminal-linux-aarch64
sha256sum -c SHA256SUMS
chmod +x linkr
./linkr --version
```

```powershell
Expand-Archive .\linkr-terminal-x86_64-pc-windows-gnu.zip
cd linkr-terminal-x86_64-pc-windows-gnu
(Get-FileHash .\linkr.exe -Algorithm SHA256).Hash   # 必须与 SHA256SUMS 一致
.\linkr.exe --version
```

**杀软提示。** Windows 优先拿 `.zip`：它就是个普通压缩包，里面躺着
`linkr.exe`，没有任何会被杀软盯上的行为。另一个
`linkr-bee-terminal.ps1` 是自解压脚本 —— exe 以 base64 内嵌、由 PowerShell 解码
后启动，**这正是杀软判定木马（dropper）的指纹**，而载荷又被 base64 遮住、扫描器
静态看不到内容，所以火绒等常按木马处理（**是误报，但行为确实无法自证**）。
要用它就先加进杀软信任区，并用上面的 SHA-256 核对；exe **未做代码签名**（需要
购买证书），Windows 可能提示「未知发布者」，那是签名状态，不是查杀结果。

还没打标签时，CI（`.github/workflows/build.yml` 的 `terminal` 任务）仍会把六个
目标的产物作为 artifact 上传：Linux x86_64、i686、arm64、armv7 与 Windows
x86_64、i686——**不含 macOS**：这个仓库从来没在 macOS 上跑过一次，发一个没人测过
的二进制等于开一张兑现不了的支票。但 30 天就过期，所以上面那个 Release 才是该收藏
的链接；校验方式相同，少一行 `tar`。

### Windows

```powershell
powershell -ExecutionPolicy Bypass -File tools\build_terminal.ps1 -Bundle
```

会生成 `dist\linkr-bee-terminal.ps1` —— 一个把 `linkr.exe` 装在里面的自解压
单文件。它首次运行时解包到 `%LOCALAPPDATA%\LinkrBee\bin`，每次运行前校验
内嵌的 SHA-256，并透传参数与退出码：

```powershell
.\dist\linkr-bee-terminal.ps1 --scan
.\dist\linkr-bee-terminal.ps1 --tui
```

因为它是自解压文件，杀软可能按第 2 节说的方式误报 —— 打了就得用，才值得打。

细节见 [Rust 终端参考](../tools/linkr-cli/README.md#build)。

## 3. 第一次会话

**1. 找到配件。** 它以 `Linkr BLE UART…` 广播：

```sh
linkr --scan
```

**2. 打开 TUI。** 不带 `--name` 时会连接第一台匹配 `Linkr BLE UART*` 的设备：

```sh
linkr --tui
```

常用变体：

```sh
linkr --name "Linkr BLE UART-3" --tui      # 指定某一台配件
linkr --address AA:BB:CC:DD:EE:FF --tui    # 跳过按名扫描
linkr --uart 115200,8,n,1,n --tui          # 交接 TUI 之前先设好 UART
linkr --query-info --tui                   # 进入前先打印 @i? 诊断
```

`--tui` 本身**不再等蓝牙连上**：界面立刻打开，连接在后台进行（状态栏显示
`connecting...`）。设备没回应也只会弹一条提示 —— 界面不退出，侧栏的 `连接`
随时可以手动重试。凡是需要会话才能给出结果的旗标（`--query-info`、`--uart`、
`--loopback-test` 等）仍然先连接再进界面，和以前一样。

**3. 首次连接时把 GPIO1 拉到 GND**，然后接受配对提示。松开即可 ——
主机信息会被记住，之后连接不用再按键。

**4. 开始输入。** 有焦点时终端窗格直接收键（在别的视图里先按 `Esc`）。
复位目标板，看控制台输出。

如果一点输出都没有，先查三根线 —— 目标 TX 接配件 RX、目标 RX 接配件 TX、
共地 —— 而不是别的什么。

## 4. CLI 模式

下面这些都不需要 TUI：同一个二进制既能跑一次性命令喂脚本和 CI，也能直接
给你一个裸串行会话。`linkr --help` 列出全部旗标；这一节是常用集，打包相关
的事在[参考手册](../tools/linkr-cli/README.md#use)里。

它有三种形态：

| 形态 | 命令 | 用来做什么 |
| --- | --- | --- |
| 找到配件 | `linkr --scan`（或 `linkr scan`） | 先看一眼，或写进脚本里探活 |
| 连上、跑命令、退出 | 下面任一旗标**再加** `--no-terminal` | 脚本、CI、体检 |
| 交互会话 | 裸的 `linkr`，或加 `--tui` 接着进界面 | 日常使用 |

### 连接

| 旗标 | 作用 |
| --- | --- |
| `--name <NAME>` | 蓝牙名或前缀；默认 `Linkr BLE UART`，匹配 `Linkr BLE UART*` |
| `--address <ADDR>` | 按地址或 UUID 直连，跳过按名扫描 |
| `--scan` | 列出附近所有蓝牙设备，不管有没有名字 |
| `--timeout <SEC>` | 扫描预算，默认 `8.0` |
| `--pair` | 请求系统配对 —— 执行期间把配件的 GPIO1 按到 GND；macOS 在读取加密服务时弹系统对话框，配好之后这台主机就记住了 |
| `--lan <HOST[:PORT]>` | 走局域网 WebSocket 桥，而不是无线电 |
| `--lan-token <HEX>` · `--lan-token-file <PATH>` | 桥的 32 位十六进制令牌；[第 10 步](#10-局域网模式)讲它从哪来 |

### 管理命令

想加几个加几个。它们在连上之后、**任何终端打开之前**执行，而且**只能走
蓝牙**：经 `--lan` 时每条都回
`management commands are not available over the LAN bridge; connect over BLE to run them`。

| 旗标 | 作用 |
| --- | --- |
| `--query-info` | 发 `@i?` 并打印诊断 —— 固件、uptime、WiFi、队列 |
| `--query-uart` | 发 `@u?` 并打印串口设置 |
| `--uart <SPEC>` | 串口设成 `baud,data,parity,stop,flow`，如 `115200,8,n,1,n`（波特率 300–3000000、数据位 5–8、校验 `n`/`o`/`e`、停止位 1/2、流控 `n`/`rtscts`） |
| `--wifi-scan` | 扫配件看得见的 2.4 GHz 网络 |
| `--wifi <SSID[,PASSWORD]>` | 让配件连网 |
| `--wifi-key-file <PATH>` | 从 `PATH` 的第一行读密码；配一个光 `--wifi <SSID>` 用，密钥就不进 `argv`、也不进 shell 历史 |
| `--wifi-off` | 忘掉记住的网络 |
| `--query-wifi` | 发 `@w?` 并打印 WiFi 状态 |
| `--webdav <URL>` · `--webdav-off` · `--query-webdav` | 设置、清除、打印日志上传目标 |
| `--loopback-test [<STR>]` | 发一段载荷并要求原样回吐 —— 证明整条链路通的最便宜办法 |
| `--loopback-timeout <SEC>` | 等回显多久，默认 `3.0` |

### 输出

| 旗标 | 作用 |
| --- | --- |
| `--json` | 管理命令的回复与事件按 JSON Lines 走 stdout；敏感命令正文仍然打码 |
| `--quiet` | 去掉进度提示 —— 结果、警告、错误照常打印 |
| `--debug-io` | stderr 上打逐字节 TX/RX 轨迹 |
| `--log-file <PATH>` | 收到的每个字节都追加进文件 |

### 终端会话

| 旗标 | 作用 |
| --- | --- |
| `--no-terminal` | 连上、跑完上面的命令、退出 —— 不进 raw 模式，没有回滚缓冲 |
| `--enter <raw\|cr\|lf\|crlf>` | `Enter` 发什么，默认 `raw` |
| `--local-echo` | 本地回显 —— 目标机不回显时用 |
| `--line-mode` | 一次发一行可见字符，而不是 raw 终端 |
| `--escape <TYPE>` | 退出会话的按键，默认 `^]` |
| `--ble-write-size <BYTES>` | 限制单次 NUS 写入长度；`0`（默认）自动协商 |
| `--write-response` | 每块都写确认 —— 仅 NUS 回退路径；Reliable UART 本来就带确认 |
| `--write-delay-ms <MS>` | 块间停顿，默认 `5.0` —— 仅 NUS 回退路径 |
| `--tui` | 把连好的会话交给 TUI，和 `linkr tui` 一模一样 |
| `--yes` | 让助手自己批准自己的命令（TUI 审批代理） |

### 子命令

| 命令 | 等价于 |
| --- | --- |
| `linkr scan` | `linkr --scan` |
| `linkr tui` | `linkr --tui` |
| `linkr completion <bash\|fish\|zsh\|powershell>` | `linkr --print-completion <shell>` |

### 配方

```sh
linkr --scan                                    # 配件在不在？
linkr --query-info --no-terminal                # 打 @i? 诊断，然后退出
linkr --json --query-info --no-terminal | jq .  # 同上，机器可读
linkr --wifi-scan --no-terminal                 # 配件看得见哪些网络
linkr --wifi MySSID --wifi-key-file ./pw --no-terminal   # 密钥不进 argv
linkr --loopback-test ping --no-terminal        # 端到端自检
linkr --uart 115200,8,n,1,n --tui               # 先设串口，再开始打字
linkr --lan 192.168.1.10 --tui                  # 走局域网桥
linkr completion zsh > _linkr                   # 给你的 shell 装补全
```

它们每一个都以[退出码](#11-退出码)收尾，所以体检只要一行：

```sh
linkr --loopback-test ping --no-terminal || echo "配件没回应"
```

## 5. 界面总览

| 区域 | 位置 | 作用 |
| --- | --- | --- |
| 连接卡 | 左侧上方 | 传输方式、设备名、连接 / 断开、局域网主机与令牌 |
| 快捷发送 | 左侧中部 | `help`、`version`、`uname -a`、`df -h`、`reboot` |
| 监控 | 左侧下方 | 串口发现；`↑` `↓` 选中后按 `Enter` 查看 |
| 当前视图 | 中间 | 终端、诊断、网络或助手 |
| 状态行 | 底部 | 焦点、视图、详情，以及按键提示 |
| 通知气泡 | 右下 | 单行提示，显示 2.2 秒 |
| 通知记录 | `Ctrl+P` → *通知记录* | 最近 200 条通知，可滚动 |

所有功能也都能从命令面板里找到，所以下面的键位表是快捷方式，不是硬性要求。

## 6. 键位

这就是完整的 `F1` 帮助，按其显示顺序：

| 按键 | 动作 |
| --- | --- |
| `Ctrl+P` | 命令面板（全部动作，可搜索） |
| `F1` | 本帮助 |
| `F2` `F3` `F4` `F5` `F6` | 终端 · 诊断 · 网络 · 助手 · 文件传输视图 |
| `Ctrl+Shift+K` | 聚焦助手输入框 |
| `Ctrl+Shift+M` | 助手：选择执行模式 |
| `Ctrl+Shift+S` | 助手：AI 配置 |
| `Ctrl+Shift+N` | 助手：新建对话 |
| `Alt+Enter` | 助手：发送消息（终端能区分时 `Ctrl+Enter` 也行） |
| 侧栏中的 `↑` `↓` | 移动选中项，`Enter` 确认 |
| 快捷发送 | 侧栏 → `help` / `version` / `uname` / `df` / `reboot` |
| `Ctrl+L` | 清屏终端窗格 |
| `Ctrl+Q` | 退出（连接中时先询问） |
| `Ctrl+Up` | 聚焦侧栏 |
| `Esc` | 关闭浮层 / 返回终端 |
| `Shift+PgUp` / `Shift+PgDn` | 滚动终端回滚缓冲 |
| `Shift+Home` / `Shift+End` | 回滚缓冲：顶部 / 底部 |
| `Ctrl+Shift+R` | 为下一个按键启用一次性 `Shift` |
| `Ctrl+Shift+C` | 为下一个按键启用一次性 `Ctrl` |
| `Ctrl+Shift+A` | 为下一个按键启用一次性 `Alt` |
| `Ctrl+Shift+V` | 把剪贴板粘贴进当前输入框（否则发给设备） |
| 在终端里拖动 | 选中文字，松手即复制 |
| `Enter` | 发送一行（应用回车模式） |
| `Tab` / `Shift+Tab` | 作为 `TAB` / `CSI Z` 发送给目标 |
| `F6`..`F12` | 原样发送给目标 |
| `Ctrl+P` → `term.*` | 字号、自动滚动、回显、保存日志、复制 |
| `Ctrl+P` → `app.*` | 通知、帮助、退出、焦点切换 |

字节序列与 `web/terminal_keys.js` 一致 —— 在浏览器里行为正常的程序，
在这里也一样。

`Ctrl+Shift+V` 通过桌面自带的工具读取系统剪贴板 —— `wl-paste`（Wayland）、
`xclip` / `xsel`（X11）、`pbpaste`（macOS）、Windows 上的 PowerShell —— 和网页
工具栏按钮读 `navigator.clipboard.readText()` 同理，读不到时也弹同类提示。
这些工具一个都没装就读不到任何内容：改用你终端自己的粘贴键，它会以括号粘贴的
形式到达，落在同一个输入框里。粘贴也是浮层会放行的按键 —— 它带的是文本，
不是按键。

复制和网页工具栏按钮是同一条契约，只是把按钮换成了手势：在终端里按下、拖过要复制的
内容，松手即复制。鼠标捕获打开后，宿主终端自己的选区已经够不着，终端程序只剩两条路，
这里一次都走：一条是系统剪贴板，用的正是 `Ctrl+Shift+V` 读剪贴板的那套工具
（`wl-copy`、`xclip` / `xsel`、`pbpaste`、`clip`），它会回话；另一条是把 `OSC 52`
交给模拟器，模拟器从不回话。所以提示讲的是**实际发生了什么**，而不是想当然 —— 有
工具接住了就是「已复制 N 个字符。」，一个都没有就是「已通过 OSC 52 把 N 个字符交给
终端——它可能直接忽略。」。有些模拟器收到这段序列解析完就丢掉（GNOME Terminal 以及
所有基于 VTE 的终端，GNOME bug 795774），所以在没装剪贴板工具的桌面上，一次会话里
的第一次复制还会补上一句：系统剪贴板没有被写入 —— Wayland 上 `sudo apt install
wl-clipboard`，X11 上 `sudo apt install xclip`。双击选中指针下的整个词（哪怕只有
一个字符），`Ctrl+L` 清屏的同时也清掉选区；没有选区时，`Ctrl+P → term.copy` 把整个
可见窗格送进同样这两条路。

两条值得记住的规则：

- **一次性修饰键**（`Ctrl+Shift+R` / `+C` / `+A`）是 TUI 对网页终端按键栏的
  替代。先武装一个，再按下一个键：它只带一次该修饰键。
- **任何浮层都会吃掉全部按键。** 帮助、通知记录或对话框打开时，
  按键不会落到下面的视图上 —— 先按 `Esc`。

在视图内部，视图获得焦点后 `PgUp` / `PgDn` / `Home` / `End` 翻动该窗格；
明确要滚终端回滚缓冲时请按住 `Shift`。

## 7. 命令面板

按 `Ctrl+P` 再输入。搜索同时匹配**中英文**动作标题以及动作 id，所以
`view`、`视图`、`wifi`、`term.copy` 都能命中。完整注册表（38 个动作，按序）：

| 分类 | 动作 |
| --- | --- |
| View | `view.terminal` · `view.diagnostics` · `view.network` · `view.assistant` · `view.transfer` |
| Focus | `focus.sidebar` · `focus.terminal` · `focus.assistant` |
| Connection | `connect` · `disconnect` · `transport.toggle` · `uart.settings` |
| Terminal | `term.font_bigger` · `term.font_smaller` · `term.font_reset` · `term.autoscroll` · `term.echo` · `term.enter_mode` · `term.clear` · `term.save_log` · `term.copy` |
| Transfer | `term.transfer_send` · `term.transfer_receive` · `term.transfer_abort` |
| Diagnostics | `diag.refresh` |
| Network | `wifi.scan` · `wifi.status` · `webdav.status` |
| Assistant | `agent.ask` · `agent.mode` · `agent.settings` · `agent.new_chat` · `agent.stop` · `agent.export` |
| App | `app.help` · `app.notices` · `app.language` · `app.quit` |

`↑` `↓` 移动，`Enter` 执行，`Esc` 关闭。当前用不了的动作不会被隐藏，而是显示为
禁用 —— 比如在通过 BLE 连接前的那些 WiFi 动作 —— 试着触发时会说明原因。

## 8. 侧栏

### 连接卡

- **传输方式**在 BLE 与局域网之间切换（`◂▸` 表示可切换；连接尝试进行中与
  会话进行时都显示 `（已锁定）` —— 先等尝试结束、再断开才能切换。web 一致：
  `connecting` 期间它也会禁用传输按钮与「切换设备」）。
- **设备**是 BLE 名称或前缀。留空则匹配任意 Linkr 配件。
- **连接 / 断开连接** —— `断开连接` 会先确认。
- 局域网模式下这里换成**局域网主机**与**局域网令牌**（32 位十六进制；
  局域网鉴权关闭时留空）。

### 快捷发送

五个预设，不用离开键盘就能用。`Enter` 执行当前选中项。
`reboot` 带 `⚠` 标记并且一定先确认 —— 它是唯一会重启目标板的预设。

### 监控

串口监控引擎盯住控制台里的已知故障特征。一旦命中就出现在这里；
选中后按 `Enter`，证据会以气泡提示展示。如果显示 *监控引擎未就绪*，
说明引擎在这台机器上没能启动。

## 9. 五个视图

### F2 —— 终端

点选或聚焦窗格后直接输入。窗格会同步目标机的窗口尺寸，全屏程序需要这个。

| 操作 | 方式 |
| --- | --- |
| 放大 / 缩小字号 | `Ctrl+=` / `Ctrl+-`，或面板 `term.font_*` |
| 重置字号 | `Ctrl+0`（范围 10–28，默认 13） |
| 清屏 | `Ctrl+L` |
| 滚动回滚缓冲 | `Shift+PgUp` / `Shift+PgDn`、`Shift+Home` / `Shift+End` |
| 复制可见输出 | 面板 `term.copy`（与拖动同样的两条路） |
| 保存日志 | 面板 `term.save_log` |
| 切换本地回显 | 面板 `term.echo` |
| 切换回车模式 | 面板 `term.enter_mode`（`raw` → `cr` → `lf` → `crlf`） |

当 Bootloader 或某个 Shell 要 `\r\n` 而不是裸 `\n` 时，用回车模式。
目标机不回显你输入的内容时，打开本地回显。

### F3 —— 诊断

发送 `@i?` 并把回复渲染成网格：

| 字段 | 含义 |
| --- | --- |
| 固件 | 配件上报的版本 |
| 运行时间 | 已运行多久 |
| BLE 访问 | `开放` 或 `受限`，外加 `链路 L{n}` |
| UART 缓冲 | 桥接器中缓冲的字节 |
| WiFi | 已配网时显示当前网络 |
| 上传队列 | 等待发送的日志上传 |

视图内按键：`r` 刷新、`PgUp` / `PgDn` 滚动、`F2` 返回终端。
管理命令仅限 BLE，所以局域网会话会显示一条提示，要求改用 BLE 连接，而不是网格。

### F4 —— 网络

- **扫描**列出附近的 2.4 GHz 网络。结果出现在*扫描结果*区块，
  反馈行会告诉你找到了几个 —— 这个数字来自实时事件流，扫描过程中就会更新。
- 选中一行、输入密码，然后**连接 WiFi**。校验会拦住：没填网络名、
  名称或密码超长、名称里含逗号、以及控制字符。
- **WiFi 状态**、**WiFi 关闭**，以及 **WebDAV** 区块（目标 URL、设置、关闭、状态）
  都在同一屏。

按键：`↑` `↓` 选择、`Enter` 执行或编辑、`Esc` 返回终端。
整屏仅限 BLE —— 局域网传输方式没有管理通道。

### F5 —— 助手

| 按键 | 动作 |
| --- | --- |
| `Ctrl+Shift+K` | 聚焦输入框 |
| `Alt+Enter` | 发送 |
| `Ctrl+Shift+M` | 选择执行模式 |
| `Ctrl+Shift+S` | AI 配置 |
| `Ctrl+Shift+N` | 新建对话 |
| `Esc` | 返回终端 |

`Alt+Enter` 在任何终端都能发送：它只是一个转义字节加 Enter 字节，任何终端都会转发。
`Ctrl+Enter` 只有在支持 kitty 键盘协议的终端里才是独立按键 —— 进入 TUI 时会自动启用该协议，
在那样的终端里两种按键都是发送。终端无法编码它时（例如 GNOME Terminal / VTE），
`Ctrl+Enter` 会以普通 `Enter` 到达、插入一行；输入框里有内容时第一次这样，
TUI 会提示一次并点名 `Alt+Enter`。

没有专门的停止按键：`Ctrl+P` → **停止当前回合**（`agent.stop`）会取消本次运行，
切换执行模式或退出也会。停止无法撤回已经发出的输入 ——
在终端里用 `Ctrl-C` 中断目标程序。

三种模式及其承诺：

| 模式 | 行为 |
| --- | --- |
| 手动 | AI 提出命令建议；点击发送后才会输入到被控机 |
| Auto · 推荐 | 识别到 Shell 提示符时自动执行低风险查询，其余输入需确认；破坏性命令在任何档位都需确认 |
| Full Auto | 命令直接执行；识别出的破坏性或不可逆命令（递归/强制删除、磁盘与文件系统工具、dd、刷写与引导工具、下载内容管道进 shell、提权、递归改权限）仍需确认；该检测不覆盖脚本或间接执行中的所有操作 |

切换模式会保留对话，但会停止当前回合并取消待发输入。

第一次提问前先配置模型端点（`Ctrl+Shift+S`）：API 基础 URL、模型 ID、
API 密钥、协议（OpenAI 兼容、Anthropic、Google AI）、推理力度、上下文窗口、
最大输出 token 与按 token 计价。`Ctrl+S` 保存，`Esc` 关闭。
密钥只存在本机 —— 见[第 13 步](#13-磁盘上的设置)。
如果端点是明文 `http` 且非本地回环，对话框会在存密钥之前警告你。

`Ctrl+P` → **导出报告**会把排查结论写成 Markdown，保存到
`<配置目录>/linkr/linkr-agent-<时间戳>.md`，包含助手为这台目标机记录的任务与
笔记。没有可导出的内容时它会明说，而不是写一个空文件。

### F6 —— 文件传输

让文件在链路两头来回移动。这一屏是一张四行的表单：`Tab` / `Shift+Tab`（或 `↑` `↓`）
在行间移动，`Enter` 执行当前行的动作。

| 行 | 放什么 |
| --- | --- |
| **方向** | 发送（本机 → 设备）或接收（设备 → 本机）。`←` `→` 或 `空格` 切换 |
| **本机路径** | 要发送的文件，或接收下来的落点。`←` `→` 移动光标 |
| **设备路径** | 另一端的路径：必须是绝对路径，`~/…` 按**设备**的家目录展开，不是本机的 |
| **操作** | `检测` · `开始` · `中止` |

**检测**是预检，视图一打开就会自己跑一次：它问设备装了 `sz`、`rz`、`dd`、
`base64`、`wc`、`tr`、`sha256sum`/`shasum` 中的哪几个，并报出查到的 `lrzsz`
版本（例如 `sz (lrzsz) 0.12.21rc`）。在答案回来之前不会往设备里输入任何东西，
**开始**在此之前也一直是灰的。

两条通道，由预检结果决定：

- **ZMODEM** —— 本机的 `sz`/`rz` 对上设备里的同款。协议本身逐帧 CRC-32 校验，
  `rz` 还会核对文件长度，所以不需要再跑一遍摘要往返。
- **`dd|base64` 分页通道** —— 网页端和助手已经在用的那条（`target_files`），
  任一端没有 `lrzsz` 时就走它。

传输故意放慢：**每 25 毫秒 256 字节**（约 10 KiB/s）。突发流量会把链路上的字节
冲掉，而串口控制台没有重传可用，所以由发送端自己控速。

| 按键 | 动作 |
| --- | --- |
| **操作**行上的 `Enter` | 执行选中的动作 |
| **方向**行上的 `空格` 或 `←` `→` | 切换方向 |
| `Esc` | 中止进行中的传输；否则返回终端 |
| `PgUp` / `PgDn` | 翻动本窗格 |
| 命令面板 `term.transfer_send` / `term.transfer_receive` | 打开视图并选好方向 |
| 命令面板 `term.transfer_abort` | 在任意界面中止传输 |
| `F6` | 打开视图（保留上次的方向） |

切换到别的视图传输也会继续 —— 只有 `Esc`（先中止再退出）或 `term.transfer_abort`
会提前结束。`开始` 在移动任何一个字节之前就会拒绝已存在的目标文件和非绝对路径。

## 10. 局域网模式

配件接入 2.4 GHz 网络后就可以不用蓝牙：

```sh
linkr --lan 192.168.1.10 --lan-token <32 hex> --tui
```

也可以先用 BLE 连上，在侧栏里切换传输方式（`传输方式`，然后重连），
前提是已经知道地址。令牌依次来自 `--lan-token`、`--lan-token-file`、
`LINKR_LAN_TOKEN` —— 以及令牌库：只要 TUI 用 BLE 连过设备，前三样都可以不带。
BLE 会话建立时 TUI 会自己向设备索取令牌（`@s?`，与网页前端同一套做法），存进
`<配置目录>/linkr/lan_tokens.json`（权限 `0600`，按设备和主机分别记键），并
自动填进令牌输入框，切换传输方式即可直连。只有桥接器关闭了鉴权时才需要把该框
留空；抓到的令牌是通行凭证，凡是可能显示这类行的地方，CLI 一律打印
`token=<redacted>`。

记住：诊断、WiFi 配网和 WebDAV 仅限 BLE —— 局域网上你得到的是终端和助手，
那几个界面会解释它们为什么被禁用。

## 11. 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 成功 |
| `1` | 运行时错误（设备、传输、命令失败） |
| `2` | 用法错误 |
| `3` | 会话中途设备消失 |
| `130` | 被中断（`Ctrl+C`） |

连接中按 `Ctrl+Q` 会先确认，所以一次误按不会掉线。

## 12. 切换界面语言

`Ctrl+P` → **切换界面语言**（`app.language`）在中英文之间切换并保存。
所有界面在下一帧重新读取设置，切换立即生效。同一个设置在网页端就是
`linkr-lang`。

首次运行会从 `LC_ALL` / `LC_MESSAGES` / `LANG` 取语言，之后以保存的值为准。
想手动改，就编辑设置文件里的 `lang` —— 当然直接用命令面板更省事。

## 13. 磁盘上的设置

| 文件 | 内容 |
| --- | --- |
| `<配置目录>/linkr/tui.json` | 字号、回车模式、本地回显、传输方式、上次局域网主机、上次 BLE 地址、当前视图、自动滚动、语言 |
| `<配置目录>/linkr/agent.json` | API 基础 URL、模型、密钥、协议、计价 |
| `<配置目录>/linkr/agent_tasks.json` | 助手按目标机记录的任务 |
| `<配置目录>/linkr/agent_notes.json` | 助手按目标机记录的笔记 |
| `<配置目录>/linkr/command_policy.json` | 命令审批策略 |
| `<配置目录>/linkr/linkr-agent-*.md` | 导出的报告 |

`<配置目录>` 在 Linux 上是 `~/.config`，macOS 上是
`~/Library/Application Support`，Windows 上是 `%APPDATA%`。

例如 `~/.config/linkr/tui.json`：

```json
{
  "font_size": 14,
  "enter_mode": "raw",
  "local_echo": false,
  "transport": "ble",
  "last_lan_host": "192.168.1.10",
  "active_view": "terminal",
  "autoscroll": true,
  "lang": "zh"
}
```

`transport` 取 `ble` 或 `lan`，`enter_mode` 取 `raw`、`cr`、`lf` 或 `crlf`，
`active_view` 取 `terminal`、`diagnostics`、`network` 或 `assistant`，
`lang` 取 `en` 或 `zh`。未知或缺失的字段会回落到默认值，所以旧文件照样能用。

## 14. 故障排查

| 现象 | 检查什么 |
| --- | --- |
| `--scan` 找不到设备 | 配件已上电；主机蓝牙适配器可用；慢的话加 `--timeout 10` |
| 能连上但控制台没输出 | 目标 TX → 配件 RX、目标 RX → 配件 TX、共地；复位目标板 |
| 有输出但打字没反应 | 目标 RX → 配件 TX；除非 CTS 和 RTS 都接了，否则关掉硬件流控 |
| 配对被拒 | 连接时按住 GPIO1 到 GND，再接受提示 |
| `linkr: error:` 后面是空的 | 管理命令超时 —— Python CLI 打印的也是这个空消息（`str(TimeoutError())` 就是空），多半是配件正忙、另一个 `linkr` 占着；稍后重跑 |
| `Bluetooth adapter is busy` / `BLE link dropped` | 第二个 `linkr`（或手机 App）正在用同一台配件 —— CLI 会自己退避重试 3 次，链路已经存在就直接加入而不抢；仍失败就等一秒重跑 |
| TUI 起不来 | 需要交互式终端；不要重定向标准输入或标准输出 |
| TUI 打开了，但状态栏一直连不上 | 连接在后台进行 —— 在侧栏点「连接」重试；蓝牙慢就加 `--timeout 15`，或用 `--address` 跳过扫描 |
| 点「切换设备」只提示「连接中」 | 连接尝试进行中 —— 等它结束（LAN 最坏约 15 秒、BLE 约 8 秒）；web 在 `connecting` 期间同样禁用该按钮与传输切换 |
| WiFi 扫描报 0 个网络 | 扫描走 BLE —— 先把传输方式切到 BLE；结果在扫描进行中就会到达 |
| 助手拒绝修改自己的配置 | 有回合正在跑；先回到对话并停止 |
| AI 配置保存失败 | 系统或浏览器存储受限；先清除配置再重试 |
| 对话框里中文被裁半 | 终端字体没有 CJK 字形 —— 换一个带中文字形的字体 |

更多见 [README 常见问题](../README.zh-CN.md#常见问题)与
[开发指南](DEVELOPMENT.zh-CN.md)（构建与刷写）。

## 15. 延伸阅读

- [项目首页](../README.zh-CN.md) —— Linkr Bee 是什么、刷写、接线、配对
- [Rust 终端参考](../tools/linkr-cli/README.md) —— 构建、打包、参数、退出码、模块布局
- [开发指南](DEVELOPMENT.zh-CN.md) —— 固件构建、测试模式、Kconfig
- [蓝牙配对与恢复](BLE_PAIRING.md) —— GPIO1 授权与恢复
- [硬件需求](HARDWARE.md) —— 引脚与电气限制
- [文档索引](README.md)
