# Linkr CLI —— 给脚本与 Agent 的命令行

本目录是 **CLI 构建** 的 linkr：同一份源码、关掉界面特性编出来的小程序（压缩包约为界面版的四成），
专门给脚本与 agent 调用——不带交互界面，把一件事发到设备、拿回确定的结果、用确定的退出码说话。

程序名是 `linkr-cli`（`--help` 的 Usage 行也这么印）。它和上一级 `linux/`、`macos/`、`windows/`
里的界面版是**同一份实现**：界面版也包含这里全部的命令行能力（程序名 `linkr`）；本包的差别只是
没有那个交互界面——裸跑 `linkr-cli` 会打印用法并以 2 退出（先说要做什么），其余动作参数与输出
格式与界面版完全一致。完整教程（含界面用法）见上一级《使用教程.md》，校验和见 `../SHA256SUMS`。

> 给 Agent 的契约：确定的输入、确定的输出（`--json` 的 JSON Lines）、确定的退出码、失败可分类。
> 模型调用、审批、任务计划仍在调用方，CLI 只做“发出去、拿回来、说清楚”。

---

## 一、快速开始

```sh
# Linux / macOS：解压后运行（按机器架构选包；macOS 另有 .dmg：本目录的是这份 CLI，上一级 macos/ 的是界面版）
tar xzf linkr-cli-1.0.0-x86_64-unknown-linux-gnu.tar.gz
./linkr-cli-1.0.0-x86_64-unknown-linux-gnu/linkr-cli --version
./linkr-cli-1.0.0-x86_64-unknown-linux-gnu/linkr-cli --scan        # 附近有哪些设备
```

```powershell
# Windows：解压 .zip（右键「全部解压缩」或 7-Zip）
.\linkr-cli.exe --version
.\linkr-cli.exe --scan
```

Linux 上也可以直接用 `../linux/` 里的 `.deb` 安装包：它装的是界面版 `linkr`，命令行能力一样。

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

只跑一个动作时：连上 → 动作 → 打印 → 退出。`--cli-then-tui` 会在参数阶段被本 CLI 构建拒绝
（它要打开终端界面，而这枚程序没有），提示改用 `--no-terminal`。

## 三、常用动作（一次一个）

| 目的 | 命令 |
|---|---|
| 列出附近的蓝牙设备 | `linkr --scan`（唯一不用 `--json`、直接印表的动作：`bee` 列 = 本程序能直连的板子） |
| 读板子诊断 `@i?` | `linkr --query-info` |
| 读串口参数 `@u?` | `linkr --query-uart` |
| 读 WiFi 状态 `@w?` | `linkr --query-wifi` |
| 设置串口 | `linkr --uart 115200,8,n,1,none`　`baud,data,parity,stop,flow` |
| 连 WiFi | `linkr --wifi "MySSID,MyPassword"` |
| 连 WiFi（密码不进参数） | `linkr --wifi "MySSID" --wifi-key-file pw.txt`（读文件第一行） |
| 扫描 WiFi | `linkr --wifi-scan` |
| 忘记 WiFi | `linkr --wifi-off` |
| 回环自测（发什么收什么） | `linkr --loopback-test 414243`（发的是字面字节 `4 1 4 2 4 3`） |
| 在板子的目标上跑 shell 命令 | `linkr --exec "uname -a"`，见下节 |

> 读诊断（`@i?` 等管理命令）仅限 BLE：LAN / stdio 下会返回一行说明并以 1 退出，这不是故障。
>
> 扫描遇到板子忙（另一个 WiFi 操作还没结束，板子此时对 `@w scan` 要么不应答、要么回
> `ERR scan=-16`）会自动重试 4 次，停顿 2/4/6/8 秒；板子报"操作结束"时还会提前重试。
> 4 次都没成才报错，报的话会说重试次数。TUI 里每次等待都有一条"最多 N 秒后自动重试…"提示。

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
| 1 | 做了但结果不对：需要人工输入、超时、被拒绝；管理命令在 LAN / stdio 上也走它（那里没有管理通道） |
| 2 | 用法错误：参数拼错、缺失、互斥冲突（普通启动且链路打不开也是它） |
| 255 | 发不出去或保不住链路：连接失败、断链（带动作的调用在连接失败时走它，不是 2） |

`--capabilities --json` 自描述本版本：支持的动作用、默认超时与退出码表，一项不落
（本 CLI 构建的 `operations` 里没有 `tui`——这里没有界面可开）。

## 七、常驻会话：`--serve`

一次一个动作要**每次重新拨号**（蓝牙尤其慢）。`--serve` 只拨一次、一直用，从 stdin 逐行读
JSON 请求，适合脚本与 Agent 连续问很多件事：

```sh
linkr-cli --ble-address EE:C7:42:34:48:CF --serve
{"event":"ready","program":"linkr","version":"1.0.0","link":"ble:EE:…","ops":[…]}
{"id":1,"op":"state"}
{"event":"begin","id":1,"op":"state"}
{"event":"result","what":"state","live":false,…}
{"event":"end","id":1,"code":0}
{"id":2,"op":"exec","cmd":"uname -a"}
{"event":"begin","id":2,"op":"exec"}
…（与一次性 `--json` 相同的事件）…
{"event":"end","id":2,"code":0}
{"id":3,"op":"quit"}
{"event":"end","id":3,"code":0}
```

框架行（`ready` / `begin` / `end`）之外的字段：

| 行 | 字段 | 说明 |
|---|---|---|
| `ready` | `program` `version` `link` `ops` | 启动横幅；`ops` 是这个会话当下能服务的请求（随传输变化：LAN / stdio 下不含 `info`/`uart`/`wifi`/`wifi-off`/`wifi-scan`/`lan` 这些仅限 BLE 的项；`--capabilities` 才是本构建的全表） |
| `begin` | `id` `op` | 每个请求的开口；`id` 是可选数字，原样回显（不给就是 `null`） |
| `end` | `id` `code` | 每个请求的收口；`code` 就是一次性命令行的退出码（第六节） |

请求就是「动作的参数换成 JSON」，`op` 名与 `--` 参数一一对应：

| `op` | 字段 | 对应命令行 |
|---|---|---|
| `info` | —— | `--query-info` |
| `uart` | 给 `value`：写；不给：读 | `--uart 921600,8,n,1,none` / `--query-uart` |
| `wifi` | 给 `ssid`（+可选 `pass`）：连；不给：读 | `--wifi` / `--query-wifi` |
| `wifi-off` | —— | `--wifi-off` |
| `wifi-scan` | —— | `--wifi-scan` |
| `lan` | —— | 读桥的局域网状态（`@s?`）：`ws=` 与 token 是否已捕获；界面里是连接区的一行 |
| `exec` | `cmd`（必填）、`timeout_ms`、`idle_ms`、`retry` | `--exec` 全部参数 |
| `loopback-test` | `data`（字面字节，非十六进制）、`timeout_ms`（默认 3000） | `--loopback-test` |
| `state` | —— | 本地状态：链路是否活着、已连、计数 |
| `quit` | —— | 结束会话（会话也随 stdin 关闭而结束） |

行为约定：

- **懒拨号**：`state`、`quit`、格式错误的行都不需要链路；第一个需要链路的请求才真正连接。
  拨号失败回 `{"event":"error","code":"not_connected","message":…}`（`code` 255，会话不死），
  等板子就绪再发一次即可。
- **坏请求不致命**：不是合法 JSON、或 `op` 不认识 → `{"event":"error","code":"bad_request"}`（`code` 2）；
  参数缺失（如 `exec` 没给 `cmd`）同理；**未知字段也拒绝**，文案会点名并给出近似字段的建议
  （`unknown field "timeout" for op "exec"; did you mean "timeout_ms"?`）——不会静默回落默认值。
  会话继续读下一行。
- **请求内格式**：请求里加 `"format":"text"`，该请求内部的事件按文本输出（框架两行仍是 JSON）。
- **结束**：`quit` 或 stdin 关闭 → 退出码 0。启动时若已给 `--ble-address` 等连接参数，
  会话只记住它、不提前拨号。

## 八、安全与留档

- **token 永不回显**：桥自述里带 token 的字段上屏前一律打码；记住的 token 存在
  `~/.config/linkr/lan-tokens`（0600），不进日志、不进通知（日志里的管理回复同样打码）。
  键按**归一化后的桥地址**存放：`ws://host:80/ws`、`host:80`、`host` 是同一条；格式不合的行
  读取时丢弃并把文件重写干净。
- **密码尽量不进命令行**：`--wifi "SSID" --wifi-key-file pw.txt` 只读文件第一行。
- **留档**：`--log-file <文件>` 追加记录设备发出的每个字节；管理通道的问答也进同一份档，
  带标签以示区分——`tx command: @i?` / `rx reply: @info …`（token/密码打码后）。
  `--debug-io` 在标准错误上跟踪双向字节（管理问答同样是 `tx command:` / `rx reply:` 两行）；
  终端里运行时 trace 写在 `$TMPDIR/linkr-trace-<进程号>.log`（0600，启动时有通知告知路径）。
- 一次性试完所有查询又不想碰真机：`--bridge -` 是本程序内置的回环设备（离线跑通全流程）。
