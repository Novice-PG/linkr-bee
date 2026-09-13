# Agent 能力边界与演进计划

本文记录 AI（Agent）功能当前的能力边界、已确认的缺口和排期。实机验收记录见
[AGENT_VALIDATION.zh-CN.md](AGENT_VALIDATION.zh-CN.md)。

## 当前能力

- **工具**：15 个，全部面向目标机与证据——计划（`update_task_plan`）、探测
  （`probe_tools` / `probe_device_profile` / `probe_download_tools`）、执行
  （`run_shell_command` / `send_serial_input`）、观察（`inspect_serial_execution` /
  `monitor_serial_execution` / `wait_for_serial_output`）、证据
  （`read_serial_log` / `search_serial_log` / `get_device_status`）、下载
  （`download_to_target` / `download_to_computer`）、网页（`read_web_page`）。
  定义见 `mobile/src/pi-agent.mjs`。
- **执行策略**：手动 / 自动 / Full Auto 三档，破坏性命令在任何档位都要人工确认；
  精确白名单加位置感知的命令边界匹配，见 `web/agent_execution_policy.js`。
- **证据纪律**：工具描述与系统提示反复要求"发送成功不等于命令完成"、退出码不等于
  目标达成、恢复后先读状态再行动、不重放历史命令。
- **记忆**：任务摘要最多 20 条（按设备隔离、脱敏后存 localStorage，见
  `web/agent_tasks.js`）；设备档案与工具可用性只在本次连接内有效，重连即清。
- **模型**：单一 OpenAI 兼容 chat completions；上下文窗口、输出上限、推理档位、
  成本费率均写死（`mobile/src/pi-agent.mjs`）。预算为每问 32 轮、96 次工具调用、
  15 分钟。

## 三个结构性缺口

1. **桌面 Web 用不上**：`web/agent_runtime.js` 原本是硬关闭的桩，只有 Vite /
   Capacitor / ArkWeb 构建会替换它。而桌面 Chrome 是主要使用场景。
2. **工具够不到配件自身**：没有工具能改 UART 参数、扫/连 WiFi、配 WebDAV、读
   `@i?` 诊断、轮换 LAN 令牌或重启配件。这类诉求目前只能由用户操作面板。
3. **记忆与成本不可见**：档案不跨连接、对话不跨刷新、执行记录仅内存且上限 50；
   token 用量没有展示，成本费率恒为 0。

## 第一期：桌面 Web 可用（已完成）

- 用 esbuild 把 `mobile/src/agent-runtime.mjs` 打成浏览器可加载的 ESM 包，提交到
  `web/vendor/agent/`，与 xterm、marked、DOMPurify 的 vendor 方式一致；
  `web/agent_runtime.js` 改为按需动态加载该 bundle。
- 静态入口不需要 npm、不需要构建即可使用助手；Vite / Capacitor / ArkWeb 构建仍走
  原有别名，行为不变。
- 浏览器直连模型端点需要端点接受页面来源。2026-09 用 `OPTIONS` 预检从
  `http://127.0.0.1:8765` 实测：DeepSeek、OpenAI、Moonshot、智谱、SiliconFlow、
  Gemini、OpenRouter 都返回 `access-control-allow-origin`，可直接使用；Anthropic
  需要附加请求头 `anthropic-dangerous-direct-browser-access: true`；Groq 拒绝浏览器
  来源，这类端点才需要代理。端点拒绝时给出可读的浏览器侧原因。
- 验收：静态入口（`tools/serve_web.sh`）出现助手入口并能完成一次"提问 → 工具调用
  → 回答"；vendor 包与依赖版本一致，且有测试覆盖；纯 Web 路径不再零测试。

实现说明（2026-09-12）：

- `tools/build_agent_bundle.sh` 生成 `web/vendor/agent/{agent-runtime.js,
  LICENSES.txt,BUILD.json}`；`web/agent_runtime.js` 改为按需加载该 bundle。
- `mobile/test/agent_bundle.test.mjs` 校验摘要、浏览器可加载性、依赖版本一致性和
  导出符号；`mobile/browser-test/static_agent.spec.mjs` 在静态服务上验证助手入口、
  一次完整的模拟模型问答，以及端点不可达时的浏览器侧提示。
- `mobile/browser-test/static-server.mjs` 为静态路径提供无日志的测试服务器。
- 仍未覆盖：真实模型端点的 CORS 行为、云端厂商需要代理这一前提，只能由用户环境
  验证；`read_web_page` 的浏览器跨域限制同样如此。

## 第二期：配件管理工具组（已完成）

复用已有的管理通道（面板向 Agent 注入的 `sendControl`，与目标绑定同一条路径），
新增受审批约束的工具：

| 工具 | 作用 | 审批 |
| --- | --- | --- |
| `get_accessory_diagnostics` | 读 `@i?`：固件、UART 缓冲、WiFi、上传队列、桥接状态 | 只读，免审批 |
| `set_uart_config` | 改波特率 / 数据位 / 校验 / 停止位 / 流控 | 需审批 |
| `wifi_scan` / `wifi_connect` | 扫描并连接 2.4 GHz 网络 | 需审批，凭据只走 BLE |
| `set_webdav_target` | 配置或关闭 WebDAV 上传目标 | 需审批 |
| `reboot_accessory` | 重启配件 | 需审批，列入破坏性命令 |

验收：每个工具都要有单元测试（含被拒绝的路径），并证明工具结果可被模型引用为证据
（不是"已设置"这类无证据回执）。

实现说明（2026-09-12）：

- 实际交付 5 个工具：`get_accessory_diagnostics`、`set_uart_config`、`wifi_scan`、
  `set_wifi`（`action=connect|off`）、`set_webdav`（`action=on|off`）。协议、校验与
  回复解析在 `web/accessory_control.js`；命令发送与回读在 `web/app.js`；审批卡片
  在面板里，复用工具行。
- **`reboot_accessory` 取消**：固件没有配件重启命令（面板里的 `reboot` 预设是发给
  目标机 Shell 的）。要做需要先加 `@linkr reboot` 一类的命令。
- 除诊断外每个动作在每个执行档位都需要一次人工批准；结果由"命令回复 + 随后回读"
  组成，`applied=false` 表示配件没有报告目标值。
- WiFi 密码只经蓝牙管理通道发送，不进入工具结果、任务记录或审批卡片（用户自己
  输入的问题文本仍按原样保留）。
- AI 设置新增"附加请求头"字段（每行一个 `名称: 值`，最多 8 条），用于 Anthropic
  这类需要特殊请求头的端点；名称与值都做了校验，存储记录无法借此注入新请求行。
  相关测试：`mobile/test/agent_config.test.mjs`、
  `mobile/test/serial_agent.test.mjs`（请求头确实发给 provider）、
  `mobile/browser-test/agent_settings.spec.mjs`（保存、非法输入、实际请求携带）。
- 测试：`mobile/test/accessory_control.test.mjs`（协议与校验）、
  `mobile/test/accessory_tools.test.mjs`（工具注入与拒绝路径）、
  `mobile/browser-test/accessory.spec.mjs`（审批、拒绝、密码不外泄）。
- 仍未覆盖：真机上的 WiFi 连接与 WebDAV 上传（沿用固件侧的实机验收清单），以及
  非 BLE 传输下的行为（局域网模式下工具会明确拒绝）。

## 第三期：记忆与成本可见（已完成）

- 按目标 UUID 持久化"设备笔记"：已知怪癖、正确 UART 参数、启动行为、已装工具，
  与当前仅在内存中的档案区分（观测值仍要标时间与新鲜度）。
- 展示 token 用量与估算成本：SDK 在每条 assistant 消息上返回 `Usage`，需要按端点
  配置费率或允许用户留空。
- 执行记录导出：把命令、退出码、证据片段导出为报告，供现场排障留档。

实现说明（2026-09-12）：

- **设备笔记**（`web/agent_notes.js`）：按 `deviceIdentity` 归档，每台最多 12 条、每条
  600 字符，与任务摘要用同一套脱敏；重复内容不重复写入，超出上限淘汰最旧的一条。
  模型通过 `remember_target_note` 写入，必须附一条支撑它的观察；笔记经
  `get_device_status.notes` 回到模型，并在面板历史区列出，用户可逐条删除。
- **token 与费用**（`web/agent_usage.js`）：累计每条 assistant 消息的 `Usage`
  （含缓存命中），会话内显示输入/输出/缓存与总量；AI 设置里的单价（每 100 万
  token，可选）**单独存储**，因此改价格不会重启正在进行的对话，未填单价时只显示
  token 数。
- **报告导出**（`web/agent_report.js`）：把目标、计划、串口执行记录（命令、投递、
  退出码、证据片段）与设备笔记导出为 Markdown，同样脱敏并限长，结尾明确"退出码与
  助手结论不等于目标达成"。
- 测试：`mobile/test/agent_notes.test.mjs`、`agent_usage.test.mjs`、
  `agent_report.test.mjs`，以及 `mobile/browser-test/agent_memory.spec.mjs`（笔记写入
  → 回到模型 → 列表 → 删除；用量与费用显示；单价校验与保存；导出文件内容）。

## 治理（已完成）

- **Full Auto 时间盒**：切换到 Full Auto 后 15 分钟自动回退到 Auto，倒计时显示在档位
  标签上（`Full Auto · 12:34`），再次选择 Full Auto 即为延长；到时会取消尚未确认的
  输入，并在状态行说明"后续命令需要确认"。时限判定放在 `web/device_executor.js`
  而不是界面里，任何界面路径都无法让该档位一直挂着手动放行。
- **按目标机的命令策略**（`web/command_policy.js`，面板历史区可编辑）：
  - *总是询问*：命中即要求确认，**包括 Full Auto**；按线缆文本做不区分大小写的子串
    匹配，因此被改写成 `sh -c '…'` 的追踪命令也逃不掉。
  - *已预先批准*：只有与条目**完全相同**的命令、且 Auto 档、且输入行为空、且以单个
    回车结尾时才免确认——与内置低风险查询列表同一套条件；无法绕过破坏性命令守卫。
  - 策略只存在本机，不进入模型请求，也不会写进导出的报告；`get_device_status.commandPolicy`
    只把"已预先批准列表 + 总是询问条数"给模型，便于它选择合适的命令形式并提前说明
    需要确认。
- 测试：`mobile/test/command_policy.test.mjs`（匹配与存储）、
  `device_executor.test.mjs` 新增的时限用例（到时回退、取消待确认、重新选择延长）、
  `mobile/browser-test/agent_governance.spec.mjs`（倒计时与延长、总是询问在 Full Auto
  下仍要确认、预先批准在 Auto 下免确认）。

## 候选池（未排期）

- 多 provider 与协议（SDK 已内置 anthropic / google / bedrock / azure 等适配器与
  自定义 headers），以及 `contextWindow` / `maxTokens` / 推理档位可配。
- 只读工具并行执行、`afterToolCall` 结果改写、`addedToolNames` 动态工具。
- 目标机文件读取（分页替代 `cat` 刷屏）与本机到目标机的文件传输（需要新传输通道）。
- 应用侧验证：把 hash、字节数、服务状态等可机器判定的检查做成工具，减少模型自述。
- 后台与触发式观察（例如启动循环检测），需要不依赖提问的运行模式。
- 会话与审批的持久化：SDK 的 harness session 层（entry tree、fork、usage rows）
  目前完全未使用。
- 治理：移动端密钥存入系统凭据库（时间盒与按目标机策略已完成，见上）。

## 边界

- 扩工具时必须保持现有证据纪律：工具返回可引用证据、走审批、不自动重放、不确定就
  明说。破坏这条链的扩展会让功能看起来更强、可信度更低。
- SDK 没有 MCP、子代理和向量记忆，相关能力需要自行实现，不要按"现成可用"排期。
- 助手不应越过"下载后等用户指示"的界线，自动安装、刷写不在计划内。
