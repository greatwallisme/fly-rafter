spec: task
name: "SDK JSON-RPC 服务器入口"
tags: [requirements, generated-draft, sdk, entry, protocol]
satisfies: [REQ-FR-SDK]
---

## Intent

SDK 以协议即契约方式暴露运行时：换行分隔 JSON-RPC 2.0 over stdio；不提供官方客户端 SDK（含 Python SDK 裁剪，用户裁决）。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- 传输 MUST：换行分隔 JSON-RPC 2.0 over stdio；帧规则对齐 dsh protocol/src/transport.ts（有 id+method 为请求、仅 id 为响应、仅 method 为通知；malformed 行忽略；handler 失败变错误帧）；错误码 MUST（勘误同步，源码 transport.ts:226-237 + server.ts:374-375）：SDK 方法面恒有 handler，未知方法经 default handler 抛普通 Error 得 **-32603**；-32601 仅出现在 transport 层未装配 handler 时；handler 内部错误同为 -32603；对端错误透传 wire code + data。
- 请求方法 MUST 全集：initialize（cwd/provider/model/reasoningEffort?/maxTokens? → serverInfo: {name: 'fly-rafter-sdk-runtime', version}）；session/prompt（sessionId 未知 id 惰性创建；contentBlocks 支持文本与图片块 `{type:'image', data: base64, mimeType: png/jpeg/webp/gif}` 经 durable attachment store 准入 → messageId）；session/resume（不存在或被占用则拒绝；仅恢复不入队 prompt）；session/wait（等该会话及托管子孙全部结束；从不创建）；session/working-directory/get 与 set；shutdown（进程级；协议内无 session/close）。
- 下行通知 MUST 全集：session.event {sessionId, event}（运行时内所有会话含子孙的完整事件逐条实时下发）；session.status {sessionId, status: idle|running}；subagent.started {parentSessionId, childSessionId}；subagent.finished {provider, agentId, parentSessionId, childSessionId, status: ok|error, stopReason, lastAssistantMessage?}。
- serverInfo.name MUST 为 `fly-rafter-sdk-runtime`（wire 稳定，一经发布不改）。
- MUST NOT 提供官方客户端 SDK；MUST NOT 交付 Python SDK（协议即契约，调用方自行封装）。
- 语义细则 MUST：请求 id 形如 req 加 uuid；params 非对象坍缩为空对象；subagent.finished 仅 local 子女；max-tokens 仅 maxTokensAsSuccess 才计 ok（默认 false）；initialize 无适配器仅 deepseek-official 允许 fallback 并路由预检；wait 从不创建会话且退出有 failure 则抛；shutdown 单飞逆序 dispose（dsh server.ts:73-76,155-189,240-257,298-334,374-375）。

## Boundaries

### Allowed Changes
- crates/fr-sdk/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: initialize 返回 serverInfo
  Test: pending_req_fr_sdk_initialize_serverinfo
  当 发送 initialize 请求时
  那么 响应含 name 为 fly-rafter-sdk-runtime 的 serverInfo 与版本号

Scenario: 未知方法 -32603
  Test: pending_req_fr_sdk_32603
  当 发送未定义方法请求时
  那么 default handler 抛普通 Error，返回错误码 -32603 的错误帧（-32601 仅 transport 层未装 handler 时出现）

Scenario: handler 抛错 -32603
  Test: pending_req_fr_sdk_handler_32603
  假设 某方法 handler 内部出错
  当 该请求被处理时
  那么 返回错误码 -32603 的错误帧

Scenario: malformed 行忽略
  Test: pending_req_fr_sdk_malformed
  当 输入流中出现无法解析的行时
  那么 忽略该行并继续服务

Scenario: prompt 未知会话惰性创建
  Test: pending_req_fr_sdk_prompt
  假设 session/prompt 携带未知 sessionId
  当 处理该请求时
  那么 惰性创建会话并返回 messageId 入队回执

Scenario: resume 拒绝不存在与占用
  Test: pending_req_fr_sdk_resume
  假设 目标会话不存在或正被占用
  当 session/resume 时
  那么 拒绝该请求

Scenario: working-directory 相对解析
  Test: pending_req_fr_sdk_working_directory
  假设 session/working-directory/set 携带相对路径
  当 处理时
  那么 相对于当前工作目录解析并返回新 cwd

Scenario: wait 等待全部子孙
  Test: pending_req_fr_sdk_wait
  假设 主会话 spawn 了子代理会话
  当 对主会话调用 session/wait 时
  那么 等待主会话与全部托管子孙结束才返回

Scenario: shutdown 进程级
  Test: pending_req_fr_sdk_shutdown
  当 调用 shutdown 时
  那么 进程级关停且协议内无 session/close

Scenario: 事件与状态通知实时下发
  Test: pending_req_fr_sdk_requirement
  假设 运行时内有活动会话与子孙会话
  当 事件与状态变化时
  那么 session.event 逐条实时下发且含子孙会话，session.status 在 idle 与 running 间正确切换

Scenario: subagent 生命周期通知
  Test: pending_req_fr_sdk_subagent
  假设 主会话 spawn 子代理并结束
  当 子代理开始与结束时
  那么 下行 subagent.started 与 subagent.finished 通知

Scenario: 仓库不提供客户端 SDK 产物
  Test: pending_req_fr_sdk_sdk
  当 检查交付物清单时
  那么 不存在官方客户端 SDK 与 Python SDK 产物

Scenario: 图片块经 attachment store
  Test: pending_req_fr_sdk_attachment_store
  假设 session/prompt 的 contentBlocks 含 png/jpeg/webp/gif 图片块
  当 处理该请求时
  那么 图片经 durable attachment store 准入并进入消息

Scenario: serverInfo 名称冻结
  Test: pending_req_fr_sdk_serverinfo
  假设 SDK 服务器多次版本迭代
  当 检查 serverInfo.name 常量时
  那么 恒为 fly-rafter-sdk-runtime 且 wire 层一经发布不改动

Scenario: local 过滤与 maxTokens 结算
  Test: pending_req_fr_sdk_local_maxtokens
  假设 子代理含非 local 来源且 turn 以 max-tokens 结束
  当 下行 subagent.finished 与状态结算时
  那么 非 local 不发 finished；max-tokens 默认计 error

Scenario: SDK initialize 契约
  Test: pending_req_fr_sdk_sdk_initialize
  假设 JSON-RPC 客户端连接
  当 调用 initialize 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK session/prompt 契约
  Test: pending_req_fr_sdk_sdk_session_prompt
  假设 JSON-RPC 客户端连接
  当 调用 session/prompt 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK session/resume 契约
  Test: pending_req_fr_sdk_sdk_session_resume
  假设 JSON-RPC 客户端连接
  当 调用 session/resume 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK session/wait 契约
  Test: pending_req_fr_sdk_sdk_session_wait
  假设 JSON-RPC 客户端连接
  当 调用 session/wait 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK session/working-directory/get 契约
  Test: pending_req_fr_sdk_sdk_session_working_directory_get
  假设 JSON-RPC 客户端连接
  当 调用 session/working-directory/get 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK session/working-directory/set 契约
  Test: pending_req_fr_sdk_sdk_session_working_directory_set
  假设 JSON-RPC 客户端连接
  当 调用 session/working-directory/set 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK shutdown 契约
  Test: pending_req_fr_sdk_sdk_shutdown
  假设 JSON-RPC 客户端连接
  当 调用 shutdown 时
  那么 行为与 PRD 18.4 表逐字一致

Scenario: SDK 通知 session.event
  Test: pending_req_fr_sdk_sdk_session_event
  假设 运行时内会话活动
  当 触发 session.event 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

Scenario: SDK 通知 session.status
  Test: pending_req_fr_sdk_sdk_session_status
  假设 运行时内会话活动
  当 触发 session.status 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

Scenario: SDK 通知 subagent.started
  Test: pending_req_fr_sdk_sdk_subagent_started
  假设 运行时内会话活动
  当 触发 subagent.started 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

Scenario: SDK 通知 subagent.finished
  Test: pending_req_fr_sdk_sdk_subagent_finished
  假设 运行时内会话活动
  当 触发 subagent.finished 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

Scenario: initialize 参数校验
  Test: pending_req_fr_sdk_initialize
  假设 initialize 分别携带空串 reasoningEffort、非正或非整数 maxTokens
  当 处理时
  那么 两种情形均报错；合法值正常受理；无适配器时仅 provider 为 deepseek-official 允许惰性挂载并做路由预检

Scenario: wait 未知 id 报错
  Test: pending_req_fr_sdk_wait_id
  假设 session/wait 携带从未存在的 sessionId
  当 处理时
  那么 报错且不创建任何会话；等待循环在 whenIdle 且子孙完成且事件序未变时才退出

Scenario: loader 就绪与 shutdown 退出序
  Test: pending_req_fr_sdk_loader_shutdown
  假设 初始化请求先于插件装载完成到达、随后调用 shutdown
  当 结算时
  那么 initialize 先等待 loader 就绪再处理；shutdown 先行响应，随后 flush 持久化、按逆序 dispose、失败聚合后 exit 0

## Questions

- Source trace: doc/prd.md#18.4: SDK 协议契约级规格（传输、方法表、通知、不提供官方客户端）, confidence: high（契约级）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
