---
kind: requirement
id: REQ-FR-ACP
title: "ACP 服务器入口"
status: accepted
liveness: auto
tags: [acp, entry, protocol]
---

## Problem

ACP 让外部编辑器/客户端接入。fly-rafter 遵循 ACP v1（对齐 dsh @agentclientprotocol/sdk@1.4.0 行为），stdio JSON-RPC，一连接多会话并行。

## Requirements

- [REQ-FR-ACP-1] MUST 遵循 ACP v1：stdio JSON-RPC，stdout 只承载协议流量；一连接多会话并行；`agentInfo` 为 `fly-rafter-acp/0.0.1`。
- [REQ-FR-ACP-2] 方法面 MUST 对齐 dsh packages/acp/acp/README.md Protocol contract：initialize（通告 v1 + session/list/resume/close + Streamable HTTP MCP）、authenticate（直接成功）、session/new（新建持久会话；workspace 与 stdio/HTTP MCP 先校验后发布；返回完整配置选项态）、session/list（持久可恢复根会话 newest-first 分页）、session/resume（恢复非活动持久会话，不重放旧更新）、session/close（静默取消 → 排干更新 → 子孙处置 → 持久化 flush → 处置该会话 scope）、session/set_config_option（model / reasoning_effort，返回完整结果态）、session/prompt（每会话同时仅一个 in-flight prompt；text/resource-link/受支持图片；settlement 等根与子孙全部完成）、session/cancel 与 $/cancel_request（未知 id no-op）。
- [REQ-FR-ACP-3] 下行 MUST：session/update（每会话串行：已提交助手消息/thought、通用工具生命周期、配置变化、context 用量；不含原始 provider delta/重试/专有呈现）；session/request_permission（一次性 allow/reject，客户端可自动应答）。
- [REQ-FR-ACP-4] 映射 MUST：一个 ACP session = 一个持久 agent+session；模型选择在 prompt 准入时快照并 pin 整个 turn。
- [REQ-FR-ACP-5] MUST NOT 支持（与 dsh 一致）：session/load、删除、fork、额外目录、SSE/ACP-transport MCP、modes、commands、plans、terminals、客户端文件系统操作、elicitation。
- [REQ-FR-ACP-6] 配置 MUST：provider / model / `session-list-page-size`（默认 100）。

- [REQ-FR-ACP-7] 生命周期细则 MUST：image 能力条件通告且 audio 与 embeddedContext 恒 false；resume 要求 cwd realpath 物理等同并优先日志记录的路由；list 游标 base64url canonical 防伪并支持 cwd 过滤；prompt 准入期两次 agent 存活校验；config id 仅 model 与 reasoning_effort；更新经 outputTail promise 链每会话串行、交付失败仅记 outputError 不阻断；settlement 循环 race（whenIdle 对 cancelled）（dsh index.ts:180-335,479-506; session.ts:78-94,246-565）。
## Scenarios

Rule: REQ-FR-ACP-1 — 传输与 agentInfo

场景: initialize 通告与 stdout 纯净
  当 ACP 客户端执行 initialize 时
  那么 响应通告 v1 与能力面，agentInfo 为 fly-rafter-acp 斜杠版本，且 stdout 无协议外流量

Rule: REQ-FR-ACP-2 — 方法面

场景: session/new 先校验后发布
  假设 客户端携带 workspace 与 stdio/HTTP MCP 配置
  当 调用 session/new 时
  那么 先校验后发布，返回完整配置选项态

场景: 每会话仅一个 in-flight prompt
  假设 某会话已有进行中的 prompt
  当 第二个 session/prompt 到达该会话时
  那么 第二个被拒绝

场景: cancel 未知 id no-op
  当 session/cancel 或 $/cancel_request 携带未知 id 时
  那么 no-op 且不报错

场景: set_config_option 返回结果态
  当 session/set_config_option 修改 model 或 reasoning_effort 时
  那么 返回修改后的完整结果态

场景: resume 不重放旧更新
  假设 持久会话已有历史更新
  当 session/resume 时
  那么 恢复成功且不重放 resume 前的更新

场景: close 排干并处置
  假设 会话正在运行且有子代理
  当 session/close 时
  那么 静默取消、排干更新、处置子孙、flush 持久化并处置该会话 scope

Rule: REQ-FR-ACP-3 — 下行更新

场景: session/update 每会话串行
  假设 同一会话产生多个更新
  当 下行时
  那么 按序串行下发，且不含原始 provider delta 与重试细节

场景: request_permission 一次性
  假设 工具需要审批
  当 session/request_permission 下行时
  那么 客户端一次性 allow 或 reject 即结算

Rule: REQ-FR-ACP-4 — 模型 pin

场景: 模型快照 pin 整个 turn
  假设 prompt 准入时选中模型 A
  当 turn 运行中配置变化时
  那么 本 turn 仍使用模型 A

Rule: REQ-FR-ACP-5 — 不支持清单

场景: 不支持的方法被拒
  当 调用 session/load、modes、commands 或 elicitation 等不支持面时
  那么 返回不支持，不提供部分实现

Rule: REQ-FR-ACP-6 — 配置

场景: session/list 分页与页大小
  假设 存在多个持久根会话且页大小配置为默认 100
  当 调用 session/list 时
  那么 newest-first 返回且受页大小约束

Rule: REQ-FR-ACP-2 — authenticate 与多会话

场景: authenticate 直接成功
  当 客户端调用 authenticate 时
  那么 直接成功返回

场景: 一连接多会话并行
  假设 同一连接已建立两个会话且会话 A prompt 进行中
  当 会话 B 发起 prompt 时
  那么 B 正常执行，不受 A 阻塞

Rule: REQ-FR-ACP-7 — 生命周期细则

场景: resume 的 cwd 与路由
  假设 持久会话记录于另一 cwd 或记录过显式模型路由
  当 resume 时
  那么 cwd 物理不等同即拒绝，恢复后优先日志记录的路由

场景: 游标防伪
  假设 客户端提交被篡改的 list 游标
  当 session/list 分页时
  那么 canonical 校验失败即拒绝

Rule: REQ-FR-ACP-2 — 方法面枚举（枚举生成）

场景: ACP initialize 契约
  假设 标准 ACP 客户端连接
  当 调用 initialize 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP authenticate 契约
  假设 标准 ACP 客户端连接
  当 调用 authenticate 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/new 契约
  假设 标准 ACP 客户端连接
  当 调用 session/new 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/list 契约
  假设 标准 ACP 客户端连接
  当 调用 session/list 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/resume 契约
  假设 标准 ACP 客户端连接
  当 调用 session/resume 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/close 契约
  假设 标准 ACP 客户端连接
  当 调用 session/close 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/set_config_option 契约
  假设 标准 ACP 客户端连接
  当 调用 session/set_config_option 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/prompt 契约
  假设 标准 ACP 客户端连接
  当 调用 session/prompt 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/cancel 契约
  假设 标准 ACP 客户端连接
  当 调用 session/cancel 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP $/cancel_request 契约
  假设 标准 ACP 客户端连接
  当 调用 $/cancel_request 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

Rule: REQ-FR-ACP-7 — 准入与过滤细则

场景: resume 拒绝形态
  假设 目标会话分别处于活跃或 activating、为子代理会话、为 fork 会话
  当 session/resume 时
  那么 三种形态均以 invalidParams 拒绝；仅非活动持久根会话可恢复

场景: list 过滤与同刻决胜
  假设 会话集合含活跃、子代理、fork、cwd 缺失或非绝对与会话
  当 session/list 时
  那么 上述全部被过滤；createdAt 降序、同刻按 id 字节序决胜

场景: prompt 图片准入
  假设 prompt 含图片而能力未通告 image、或 base64 非 canonical
  当 准入校验时
  那么 以 invalidParams 拒绝；单 in-flight 冲突亦为 invalidParams

场景: request_permission 门控与排干
  假设 权限请求非本会话所有或缺 callId
  当 拦截结算时
  那么 不下行客户端、按 unavailable fail-closed；合法请求先 drainUpdates 再下行，选项为 allow-once 与 reject-once；configOptions 请求亦经 outputTail 串行化，未知 config id 报错

## Dependencies

- REQ-FR-AGENT-LOOP
- REQ-FR-SESSION

## Source Trace

- doc/prd.md#18.3: ACP 协议契约级规格
- confidence: high（契约级；方法字段细节以 dsh ACP README 为权威）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
