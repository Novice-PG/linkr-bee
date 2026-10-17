# Linkr CLI —— 给脚本与 Agent 的命令行

本目录是 **CLI 形态** 的 linkr：不带界面，把一件事发到设备、拿回确定的结果、用确定的退出码说话。

它与上一级 `linux/`、`macos/`、`windows/` 里的界面版（TUI）**是同一枚程序**：直接运行 `linkr` 打开界面；
带上动作参数（如 `--query-info`、`--exec "…"`）就是命令行工具。本目录内是与平台文件夹**逐字节相同**的
压缩包副本，方便只要命令行的人单独取用；完整教程（含界面用法）见上一级《使用教程.md》，校验和见
`../SHA256SUMS`（同一文件名即同一哈希）。

> 给 Agent 的契约：确定的输入、确定的输出（`--json` 的 JSON Lines）、确定的退出码、失败可分类。
> 模型调用、审批、任务计划仍在调用方，CLI 只做“发出去、拿回来、说清楚”。

---

## 一、快速开始

```sh
# Linux / macOS：解压后运行（按机器架构选包）
tar xzf linkr-1.0.0-x86_64-unknown-linux-gnu.tar.gz
./linkr-1.0.0-x86_64-unknown-linux-gnu/linkr --version
./linkr-1.0.0-x86_64-unknown-linux-gnu/linkr --scan        # 附近有哪些设备
```

```powershell
# Windows：解压后（自带 tar.exe 或 7-Zip）
.\linkr.exe --version
.\linkr.exe --scan
```

Linux 上也可以直接用 `../linux/` 里的 `.deb` 安装包，装完 CLI 与界面同在一个 `/usr/bin/linkr`。

## 二、怎么连

| 连接方式 | 参数 |
|---|---|
| 蓝牙地址（最稳） | `--ble-address EE:C7:42:34:48:CF` |
| 按名字片段找 | `--ble-name "UART-3"` |
| 局域网 WebSocket | `--lan-host 192.168.0.102`（也可给完整 `ws://` 地址） |
| 该桥要求的 token | `--lan-token <TOKEN>`；或 `--lan-token-file <文件>`；或环境变量 `$LINKR_LAN_TOKEN` |
| 锁定传输方式 | `--transport ble\|lan\|stdio` |
| 离线回环（试手） | `--bridge -` |
| 连接超时 | `--connect-timeout <秒>`（默认 20；蓝牙链路最多再等 30） |

只跑一个动作时：连上 → 动作 → 打印 → 退出。`--cli-then-tui` 会跑完动作再留在界面里。

## 三、常用动作（一次一个）

| 目的 | 命令 |
|---|---|
| 列出附近的蓝牙设备 | `linkr --scan`（唯一不用 `--json`、直接印表的动作：`bee` 列 = 本程序能直连的板子） |
| 读板子诊断 `@i?` | `linkr --query-info` |
| 读串口参数 `@u?` | `linkr --query-uart` |
| 读 WiFi 状态 `@w?` | `linkr --query-wifi` |
| 读上传状态 `@d?` | `linkr --query-webdav` |
| 设置串口 | `linkr --uart 115200,8,n,1,none`　`baud,data,parity,stop,flow` |
| 连 WiFi | `linkr --wifi "MySSID,MyPassword"` |
| 连 WiFi（密码不进参数） | `linkr --wifi "MySSID" --wifi-key-file pw.txt`（读文件第一行） |
| 扫描 WiFi | `linkr --wifi-scan` |
| 忘记 WiFi | `linkr --wifi-off` |
| 设置上传地址 | `linkr --webdav http://host:port/upload` |
| 关掉上传 | `linkr --webdav-off` |
| 回环自测（发什么收什么） | `linkr --loopback-test 414243`（十六进制） |
| 在板子的目标上跑 shell 命令 | `linkr --exec "uname -a"`，见下节 |

> 读诊断（`@i?` 等管理命令）仅限 BLE：LAN / stdio 下会返回一行说明并以 1 退出，这不是故障。

## 四、`--exec`：把命令送到板子的目标 shell

默认用 **rc-marker** 判定结束：先把你的命令改写成

```sh
你的命令; echo "LINKR_RC:$?"
```

再追一个回车发出，读到 `LINKR_RC:<状态>` 即知命令自身的退出码。也可用 `--idle <秒>`：
输出安静这么久就当作结束。

| 参数 | 作用 |
|---|---|
| `--timeout <秒>` | 整条命令的最长运行时间（默认 30） |
| `--idle <秒>` | 换成“安静即结束”的判定方式 |
| `--rc-marker` | 显式指定 rc-marker 判定（默认） |
| `--retry <n>` | 超时后先探一次状态，必要时最多重发 n 次 |
| `--dry-run` | 只打印将要发出的字节，不连接、不发送 |

```sh
linkr --ble-address EE:C7:42:34:48:CF --exec "uname -a"
linkr --bridge - --no-terminal --exec "echo hi" --json --quiet   # 离线试跑
```

提示：`--exec` 靠回显里的 `LINKR_RC:` 行工作；若目标 shell 会把光标控制序列混进回显，
优先用 `--idle`。命令里请自带引号处理，CLI 原样转发。

## 五、`--json`：给程序读的输出

`--json` 时每行一个 JSON 事件（JSON Lines）：

| 事件 | 字段 | 含义 |
|---|---|---|
| 启动横幅 | `{"event":"output","text":"…"}` | 连接建立后设备先说的话 |
| 运行中输出 | `{"event":"output","t":毫秒,"data":"…"}` | 设备实时输出，按到达分段 |
| `--exec` 开始 | `{"event":"start","execution_id":"…","link":"…","mode":"rc-marker","cmd":"…"}` | 一次 exec 的标识与判据 |
| 结果 | `{"event":"result","what":"uart","ok":true,"reply":"uart=115200"}` | 查询成功；`ok:false` 时看 `detail` |
| exec 结束 | `{"event":"result","ok":true,"rc":0,"elapsed_ms":5,"bytes":58,…}` | 带目标退出码 |
| 出错 | `{"event":"error","code":"timeout","message":"…","still_running":true,…}` | 失败可分类的依据 |

`code` 的稳定取值：`needs_input`（命令在等人输入）、`timeout`、`link_lost`、`not_connected`。
`--quiet` 只关掉 stderr 上的进度行，JSON 照常。连接失败时 stderr 会有一行 `error: …`，
退出码见下节。

## 六、退出码

| 退出码 | 含义 |
|---|---|
| 0–125 | `--exec` 目标命令自身的状态，原样透传 |
| 126 · 127 | 保留 shell 含义（127 = 命令没找到） |
| 1 | 做了但结果不对：需要人工输入、超时、被拒绝 |
| 2 | 用法错误；查询类动作连不上链路也是它 |
| 255 | `--exec` 发不出去或保不住链路（连接失败、断链） |

`--capabilities --json` 自描述本版本：支持的动作用、默认超时与退出码表，一项不落。

## 七、安全与留档

- **token 永不回显**：桥自述里带 token 的字段上屏前一律打码；记住的 token 存在
  `~/.config/linkr/lan-tokens`（0600），不进日志、不进通知。
- **密码尽量不进命令行**：`--wifi "SSID" --wifi-key-file pw.txt` 只读文件第一行。
- **留档**：`--log-file <文件>` 追加记录设备发出的每个字节；`--debug-io` 跟踪双向字节
  （终端里运行时写在 `$TMPDIR/linkr-trace-<进程号>.log`，0600，启动时有通知告知路径）。
- 一次性试完所有查询又不想碰真机：`--bridge -` 是本程序内置的回环设备（离线跑通全流程）。
