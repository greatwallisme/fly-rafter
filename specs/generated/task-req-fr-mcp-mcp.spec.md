spec: task
name: "MCP 客户端"
tags: [requirements, generated-draft, mcp, tools]
satisfies: [REQ-FR-MCP]
---

## Intent

MCP 是外部工具接入通道。fly-rafter 需要与 dsh mcp-client 对齐的 transport 与动态工具注册。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- fr-mcp MUST 支持 stdio 与 Streamable HTTP 两种 transport：stdio 子进程环境 = 净化父环境 + 配置 env；http 携 url/headers（对齐 dsh mcp-client/index.ts:119-142、transport.ts:22-36）。
- 服务器名 MUST 匹配 [A-Za-z0-9_-]{1,32} 且实例间唯一，活跃重名装载失败 "mcp-client: serverName \"X\" is already in use..."；MCP 工具 MUST 以 raw JSON Schema 动态注册进 tools 注册表：公名 `mcp__<server>__<raw>` 归一到 64 字符上限 [A-Za-z0-9_-]，有损变更追加 `_<12hex sha256("server\0raw")>`；raw 名仅上 wire tools/call；description/inputSchema 透传（description 缺省空串）、outputSchema 仅在支持时保留。
- 资源工具 3 件 MUST 提供：list_mcp_resources / read_mcp_resource / list_mcp_resource_templates；渲染逐字 `MCP server: <server>\n<JSON>`、blob 字段替换 `[binary resource: N base64 characters; available to programmatic callers]`；系统提示词 section 'mcp-resource-servers' 文本逐字 `## MCP resource servers\n\nUse list_mcp_resources, list_mcp_resource_templates, or read_mcp_resource with one of these names as the server argument: ["a","b"].`；未知/越权 server → `MCP resource server "X" is unavailable in this agent's scope`；资源操作共用 toolCallTimeoutMs。
- 生命周期 MUST：apply 阻塞于 connection.ready（首连+同步，成败都返回）；failOnStartupError 默认 false，true → 抛 `mcp-client(<server>): initial connection or tool synchronization failed`；首次同步 registrationFailure 仅 failOnStartupError 时 throw、此后一律 contain；dispose 停重连、5s 关闭确认、等在途尝试与同步链、注销全部工具。
- 服务器指令 MUST：捕获为 `### MCP server: <serverName>\n\n<trim 文本>`（无则空串）；超 maxInstructionBytes（默认 32768）该次尝试失败；发布为系统提示词 section `mcp:<server>`、order MCP_SERVERS、interpolate:false。
- 重连 MUST：默认 enabled、initialDelayMs 500、maxDelayMs 30000、maxAttempts 10；延迟 = min(maxDelayMs, initialDelayMs × 2^(失败次数−1))；预算仅在连接存活 ≥ maxDelayMs（稳定窗）后掉线才重置；超限 → 注销全部工具、清指令、日志 `giving up after 10 consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect`，仅插件重载可再连；enabled=false → 工具持续失败或永不注册；失败尝试 5s 内无法确认 transport 关闭 → 停止重连；断连期间工具保持注册、资源请求抛 `mcp-client(<server>): server is disconnected`、死代在途调用作废；重连成功日志 `reconnected and re-synced tools (attempt N/M)`。
- 工具同步 MUST 两段式：先取完整新代（失败旧代不动）；raw 重名 → `server listed tool "X" more than once — invalid tool list`；换装先 dispose 旧代再注册新代；注册表冲突回滚该服务器全部工具并 contain（启动时 throw）；tools/list_changed 触发串行重同步（cacheMode 'refresh'）。
- 工具调用 MUST：toolCallTimeoutMs 默认 60000 并随执行信号；超时 → isError 结果；MCP isError 结果抛出提取文本；非法 CallToolResult → `Tool "<raw>" returned an invalid MCP result: <issues>`；task-required 工具 → `Tool "<raw>" requires task-based execution, which this bridge does not support`；非对象模型参数坍缩为 {}。
- 内容投影 MUST 逐字：text 块 '\n' 连接；resource_link → `Resource link: <name> (<uri>)`（缺名或 uri → `[resource link unavailable: the MCP block is missing its name or URI]`）；audio → `[audio result unsupported: <mime>; raw audio data remains available to programmatic callers]`；embedded → `[embedded resource unsupported; raw resource data remains available to programmatic callers]`；未知类型 → `[unsupported MCP content type: <type>]`；非对象块 → `[unsupported MCP content block: expected an object]`；空内容 → `(<toolName> returned no model-visible content)`；图片仅 PNG/JPEG/WebP/GIF canonical base64 且模型路由声明 image input 才准入，拒绝投影 `[image unavailable: <mime>; <reason>; raw image data remains available to programmatic callers]` 且 canonical 值保留原始数据。

## Boundaries

### Allowed Changes
- crates/fr-mcp/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: stdio 服务器连接
  Test: pending_req_fr_mcp_stdio
  假设 配置了一个 stdio MCP 服务器
  当 建立连接并完成握手时
  那么 连接就绪且子进程环境为净化父环境加配置 env，可列举工具

Scenario: Streamable HTTP 服务器连接
  Test: pending_req_fr_mcp_streamable_http
  假设 配置了一个 Streamable HTTP MCP 服务器
  当 建立连接并完成握手时
  那么 连接就绪且请求携带配置 url 与 headers，可列举工具

Scenario: 工具以命名空间前缀注册
  Test: pending_req_fr_mcp_requirement
  假设 已连接一个 MCP 服务器
  当 列举其工具时
  那么 工具以命名空间前缀按 raw JSON Schema 动态注册进 ToolRegistry

Scenario: 资源工具三件可用
  Test: pending_req_fr_mcp_requirement
  当 分别调用 list_mcp_resources、read_mcp_resource、list_mcp_resource_templates 时
  那么 三者均返回对应资源数据且渲染逐字 MCP server: <server> 前缀格式

Scenario: 资源工具越权与提示词段
  Test: pending_req_fr_mcp_requirement
  假设 agent scope 不含某服务器而调用其资源
  当 结算时
  那么 报 MCP resource server "X" is unavailable in this agent's scope，且系统提示词含逐字 mcp-resource-servers 段

Scenario: stdio 工具命名模式
  Test: pending_req_fr_mcp_stdio
  假设 已连接 stdio 服务器 server-a 且其工具名为 raw-tool
  当 注册进 ToolRegistry 时
  那么 注册名严格为 mcp__server-a__raw-tool

Scenario: server 名约束
  Test: pending_req_fr_mcp_server
  假设 服务器名含非法字符或超过 32 字符
  当 配置加载时
  那么 拒绝（须 [A-Za-z0-9_-]{1,32} 且实例间唯一）

Scenario: 长名有损归一
  Test: pending_req_fr_mcp_requirement
  假设 server 加 raw 拼接超 64 字符或含非法字符
  当 生成公名时
  那么 归一到合法形态并追加 _加 12hex sha256 后缀，raw 名仅在 wire tools/call 使用

Scenario: 启动失败双形态
  Test: pending_req_fr_mcp_requirement
  假设 首连失败且分别以 failOnStartupError false 与 true 部署
  当 apply 结算时
  那么 false 时含错返回不影响装配其余部分，true 时抛 mcp-client(<server>): initial connection or tool synchronization failed

Scenario: dispose 语义
  Test: pending_req_fr_mcp_dispose
  假设 插件被卸载
  当 dispose 结算时
  那么 停止重连、5s 关闭确认、等待在途尝试与同步链、注销全部工具

Scenario: 指令捕获与超限
  Test: pending_req_fr_mcp_requirement
  假设 服务器返回 instructions 且长度逼近上限
  当 发布提示词段时
  那么 以逐字 ### MCP server 头发布为 mcp:<server> 段（interpolate:false），超 32768 字节该次尝试失败

Scenario: 退避与稳定窗
  Test: pending_req_fr_mcp_requirement
  假设 连续 3 次连接失败
  当 计算重试延迟时
  那么 依次 500ms、1000ms、2000ms（封顶 30000ms）；仅存活满 30000ms 后掉线才重置计数

Scenario: 重连耗尽
  Test: pending_req_fr_mcp_requirement
  假设 连续失败超过 10 次
  当 结算时
  那么 注销全部工具并清指令，日志 giving up after 10 consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect

Scenario: 断连期间调用
  Test: pending_req_fr_mcp_requirement
  假设 服务器断连且重连进行中
  当 调用其工具与资源接口时
  那么 工具保持注册但调用失败，资源请求抛 mcp-client(<server>): server is disconnected，死代在途调用被作废

Scenario: 两段式同步与回滚
  Test: pending_req_fr_mcp_requirement
  假设 新代获取失败与注册表冲突两种故障
  当 同步结算时
  那么 取代失败旧代不动；注册冲突回滚该服务器全部工具并 contain

Scenario: 重复工具名拒绝
  Test: pending_req_fr_mcp_requirement
  假设 服务器列表返回同名工具两次
  当 同步时
  那么 报 server listed tool "X" more than once — invalid tool list

Scenario: list_changed 触发重同步
  Test: pending_req_fr_mcp_list_changed
  假设 服务器发 tools/list_changed 通知
  当 处理时
  那么 串行重同步且 cacheMode 为 refresh，不打断在途调用

Scenario: 超时与错误投影
  Test: pending_req_fr_mcp_requirement
  假设 调用超过 60000ms 默认与服务器返回 isError 两种情形
  当 结算时
  那么 超时产生 isError 结果；isError 抛出提取文本作为错误结果

Scenario: 非法结果与 task 工具
  Test: pending_req_fr_mcp_task
  假设 服务器返回不合 schema 的 CallToolResult 与声明 task 必需的工具
  当 结算时
  那么 分别报 Tool "<raw>" returned an invalid MCP result 与 requires task-based execution 文案

Scenario: 非对象参数坍缩
  Test: pending_req_fr_mcp_requirement
  假设 模型对某 MCP 工具提交非对象参数
  当 调用时
  那么 参数坍缩为 {} 上 wire

Scenario: 特殊内容块逐字投影
  Test: pending_req_fr_mcp_requirement
  假设 结果分别含 resource_link、audio、embedded resource、未知类型、非对象块
  当 投影时
  那么 五种情形各自产出逐字占位文案，text 块以换行连接

Scenario: 图片准入门控
  Test: pending_req_fr_mcp_requirement
  假设 图片结果分别为合法 canonical base64 且模型支持、与模型不支持 image input
  当 投影时
  那么 前者以图像内容准入，后者投影逐字 image unavailable 占位且 canonical 值保留原始数据供程序调用方

Scenario: 空内容占位
  Test: pending_req_fr_mcp_requirement
  假设 工具成功返回零内容块
  当 投影时
  那么 产出 (<toolName> returned no model-visible content)

## Questions

- Source trace: doc/prd.md#14: transport、命名空间、动态注册、资源工具 3 件, confidence: high
- Replace pending test selectors with real test names before lifecycle verification.
