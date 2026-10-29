# A2A：常驻 Linkr 助手

Android App 可以在本机托管一个常驻 AI 助手，并通过
[Agent2Agent（A2A）协议](https://a2a-protocol.org/) 把它开放给其他 agent。这是
agent-to-agent 的一半：外部 agent 委托任务，而不是逐个调用串口工具；另一半是现有
的 MCP 服务器。两者共用同一个 App、端口和 Bearer 令牌。

> 状态：协议、任务管理和传输已实现，并有主机 JVM、JavaScript 与浏览器测试覆盖。
> 真机验证（2026-10-29，Android 16）：局域网可拉取 agent card；`SendMessage`
> 弹出一次确认后，常驻任务在手机上对 mock OpenAI 兼容端点执行并返回
> `TASK_STATE_COMPLETED` 与 artifact；`GetTask`/`ListTasks` 与 MCP
> `start_agent_task` 路径也验证过。该轮目标板未连接（板子广播了新地址，需要 GPIO1
> 配对），因此助手对目标串口的工具未在真机执行。一次后台确认尝试中任务完成但阻塞式
> HTTP 响应丢失，后台任务建议用 `returnImmediately:true` + `GetTask` 轮询。

## 前提

1. Linkr Bee App 正在运行，且已开启 **允许局域网内的 AI 客户端连接**（MCP 服务）。
   A2A 端点共用同一个监听端口和令牌。
2. App 里有可用的 AI 配置。**板载 AI 配置不会自动使用**：用户必须先在 AI 设置里
   点 **从板载载入**，把配置写进本机设置。没有可用配置时，`get_agent_status` 返回
   `available:false`，任务会被拒绝。
3. 除非访问模式是 **完全授权**，否则首个任务需要在手机上确认一次。**每条命令**与
   **仅高危**模式下，App 会在任务启动时弹一次确认。

## 发现

| 内容 | 地址 |
| --- | --- |
| Agent Card | `http://<手机 IP>:8712/.well-known/agent-card.json` |
| JSON-RPC 端点 | `http://<手机 IP>:8712/a2a` |
| 鉴权 | `/a2a` 请求带 `Authorization: Bearer <MCP 令牌>` |

Agent Card 是公开的（不含密钥）。它声明 JSON-RPC 接口、Bearer 鉴权、不支持流式和
服务端推送。客户端从 App 的 MCP 设置卡片获取令牌，与 MCP 相同。

## 支持的方法

实现 A2A v1.0 JSON-RPC 绑定：

| 方法 | 结果 |
| --- | --- |
| `SendMessage` | `{"task": Task}`；启动常驻任务（先完成一次确认） |
| `GetTask` | `Task`；任务运行期间轮询 |
| `ListTasks` | `{"tasks": [...], "nextPageToken": "", "pageSize": n, "totalSize": n}` |
| `CancelTask` | `Task`；取消排队或运行中的任务 |
| `GetExtendedAgentCard` | 与公开 card 相同 |

`SendStreamingMessage` / `SubscribeToTask` 返回明确的 `UNSUPPORTED_OPERATION`：
原生传输是“一个 POST 一个响应”，不保持 SSE 长连接。Card 里
`capabilities.streaming:false`，符合规范的客户端会改用 `GetTask` 轮询。

`SendMessage` 只接受文本 part；文件、URL、结构化数据会被明确拒绝而不是静默丢弃，
因为目标是串口控制台，不是文档仓库。`configuration.returnImmediately` 会被遵守；
默认阻塞，但串口任务可能超过任何 HTTP 超时，所以服务端最多等 30 秒，然后返回
运行中的 task 让客户端继续轮询。

### 示例：启动并轮询

```sh
TOKEN=<App 里的 64 位十六进制令牌>

curl -s -X POST http://192.168.1.42:8712/a2a \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"role":"ROLE_USER","parts":[{"text":"检查目标内核版本并报告 uptime。"}],"messageId":"msg-1","contextId":"bench-1"}}}'

curl -s -X POST http://192.168.1.42:8712/a2a \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"<task id>"}}'
```

完成的任务把助手答复放在 artifact 里：

```json
{"jsonrpc":"2.0","id":2,"result":{
  "id":"agent-…","contextId":"bench-1",
  "status":{"state":"TASK_STATE_COMPLETED"},
  "artifacts":[{"artifactId":"artifact-…","name":"Linkr Bee resident agent answer",
                "parts":[{"text":"Kernel 6.1.0 …"}]}]
}}
```

## 确认与任务执行

- 启动任务就是该任务的 **一次性授权**。**每条命令**模式（以及 **仅高危**模式，启动常驻
  agent 被视为高危）会在手机上弹一次确认；拒绝则什么都不启动。**完全授权**不弹确认；
  **只读**模式拒绝任务。
- 任务内，助手的串口写入与配件修改不再逐条弹确认；但仍走 App 原有执行器、证据纪律和
  单写入闸门。任务运行期间，外部 MCP 的写工具会被明确拒绝，避免争抢串口。
- 同时只运行一个任务，最多 8 个排队。默认**没有墙钟硬超时**：模型慢、目标启动要
  几分钟时，不应因为计时器到点而中断正在推进的任务。需要期限的调用方可在
  `metadata.timeoutMs` 里设置（最长 24 小时，0 表示不设）；停止长任务的正常方式是
  `CancelTask`（MCP：`cancel_agent_task`）。助手面板的“停止”会取消当前及所有排队任务。
  常驻任务执行时显示状态，并阻止页面助手同时提交；任务退出后恢复。关闭 MCP 服务也会取消常驻任务。
- 排队任务绑定提交时批准的设备与连接会话；执行前换机或重连会令旧任务失败，需要针对当前设备重新提交。
  A2A 与 MCP 共用审批保护：关闭服务、更换令牌或访问模式、原生请求取消都会撤销待确认请求。
- 会话：A2A 的 `contextId`（MCP 的 `sessionId`）续聊同一会话；省略则用默认会话，
  MCP 的 `newSession:true` 或新的 `contextId` 开新会话。板载提示词在会话开始时读取一次，
  与 App 内新对话一致。切换目标会重置该会话历史，因为旧记录不能作为新控制台的证据。
- 任务只活在 App 进程内：退到后台由前台服务维持；进程被杀则服务停止、运行中的任务丢失。
  任务历史持久化尚未实现。

## 安全说明

- 持有 MCP 令牌的人可以启动常驻任务并消耗所配置模型的 token。请像对待 API Key 一样
  保管令牌；有疑问时在 App 里轮换。
- 确认卡片只显示请求的简短预览。请先看清再批准：任务后续的写操作不会再弹确认。
- 助手输出是模型输出，不是设备证据；不会包含 API Key 或自定义请求头，但可能出错。
  设备变更请用 MCP 读工具或串口日志复核。
- Agent Card 是公开的，但只含名称、描述和接口地址，不含 provider/model；这些信息通过
  鉴权的 `get_agent_status` 查看。
- 板载 AI 配置在板子上是明文，本功能不改变这一点；只有用户显式载入本机后才会被使用。

## MCP 等价入口

不接 A2A 的 MCP 客户端也能使用同一个常驻助手：

| MCP 工具 | 作用 |
| --- | --- |
| `get_agent_status` | 配置摘要、忙碌状态、队列、最近任务 |
| `start_agent_task` | 确认并排队一个任务 |
| `get_agent_task` | 轮询单个任务（可选 `waitMs`） |
| `list_agent_tasks` | 最近任务，可按会话过滤 |
| `cancel_agent_task` | 取消任务 |

访问模式、确认通知和设备工具见 [MCP 服务器指南](MCP.zh-CN.md)。
