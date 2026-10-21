---
kind: requirement
id: REQ-FR-SESSION
title: "会话系统（v4）"
status: accepted
liveness: auto
tags: [session, persistence]
---

## Problem

dsh 的“协议即会话日志”原则要求：模型可见即已记录，每次 LLM 请求必须能从会话日志确定性重建。fly-rafter 需要完整的 v4 会话格式：追加式 JSONL+zstd、surface/replace-op、deriveMessages、fork、abort 补偿，字段与 dsh 逐字一致。

## Requirements

- [REQ-FR-SESSION-1] fr-session MUST 实现全部 durable SessionEvent 类型与字段，与 dsh `packages/core/session/src/types.ts` 判别联合逐字一致；公共字段 type/seq/time/data/ignorable?/surfaceOp?/sourceEventSeqs?；request/header 携带 EpochHeader（config + tools + adapterDefaults，reason: initial/resume/change/series）。
- [REQ-FR-SESSION-2] fr-session MUST 只实现会话格式 v4；文件为 `session.v4.jsonl`（可选 zstd 压缩 `session.v4.jsonl.zstd`）；世代命名，永不改名或删除已发布文件；读到非 v4 文件 MUST 报明确错误；写路径 = 编码 → 校验 → 独占发布后继文件。
- [REQ-FR-SESSION-3] fr-session MUST 支持 surface 与 replace-op：压缩经 `{op:'replace', startSeq, endSeq}` 重写表面，新内容引用 sourceEventSeqs，旧内容永久留存于日志。
- [REQ-FR-SESSION-4] fr-session MUST 实现 deriveMessages：每次请求前从日志按 request/header 世代 + 表面 + 工具增删 developer/message 重建消息数组并冻结；`tool/ptc-dispatch*` 一律忽略；同输入两次重建 MUST 逐字节一致。
- [REQ-FR-SESSION-5] fr-session MUST 实现 session/fork：以 sourceEventSeqs 前缀派生新会话；fork 会话与子代理会话 MUST NOT 被 `--session-id`/`session/resume` 采纳。
- [REQ-FR-SESSION-6] abort 时 MUST 为未派发的工具调用补合成错误 `tool/result`，保证重放合法。
- [REQ-FR-SESSION-7] 重建时遇到未知必需事件 MUST 拒绝重建（ignorable: true 语义照搬 dsh）。
- [REQ-FR-SESSION-8] SessionHeader 逻辑头 MUST 为 version/id/createdAt/cwd?/parentSession?/isSeeded/origin/delegationDepth?/agentPreset?（cwd、parentSession、origin、delegationDepth、agentPreset 可选；delegationDepth 缺省即 0；origin 值只允许 'subagent'）；磁盘头 MUST 恰含六必需键 type/version/id/createdAt/isSeeded/delegationDepth；MUST NOT 定义或接受 seedLength 字段（dsh session/src/types.ts:95-131、session-persistence-jsonl/src/format.ts:84-96）。
- [REQ-FR-SESSION-9] 消息角色 System/Developer/User/Assistant/ToolResult MUST 均带 MessageSource 判别；ContentBlock MUST 为全集：text、reasoning、image、file、tool-call、tool-addition、tool-removal。
- [REQ-FR-SESSION-10] system prompt MUST 以 system/message 节点流存在于表面，支持 in-history 增量追加与 first-node 归一化两条路由；空渲染 MUST 清空活跃尾部 system 节点且 head 节点以 replace 落为空文本节点（head 亦占节点，dsh agent-loop/src/runtime-context.ts:104-110）。

- [REQ-FR-SESSION-11] 编码与发布不变量 MUST：种子信封仅六键、seq 从 0 连续；首发 wx 0600 fsync 后 link+unlink 独占发布（禁 rename）并逐级目录 fsync；append 失败 truncate 回滚；zstd 撕裂尾恢复完整记录；文件名世代与头 version 不一致报错（dsh session-persistence-jsonl/src/index.ts:1282-1461）。
- [REQ-FR-SESSION-12] surface 约束 MUST：仅 system/developer/user/assistant message 与 tool/result 五类入表面；replace 区间为表面节点双闭区间且必须早于当前 seq；sourceEventSeqs 非空、无重复、全部更小、必须覆盖全部被遮蔽节点、assistant 禁带；tool/result 替换只许改 content；node-0 只许 system/message 单节点重写（dsh surface.ts:50-56,290-507）。
- [REQ-FR-SESSION-13] abort 两级补偿 MUST：调度器跳过 = ABORTED_BEFORE_DISPATCH（AbortError）；步级恢复 = TOOL_NOT_STARTED / TOOL_OUTCOME_UNKNOWN（精确文案）；恢复 seq 从 last.seq+1 连续、time 复用最后真实事件（dsh repair.ts:15-197、tool-calls.ts:250-260）。
- [REQ-FR-SESSION-14] sourceEventSeqs 落盘 MUST 为折叠编码（连续 run 长度 3 以上折叠为 [start,end]），解码校验严格递增（dsh seq-ranges.ts:18-67）。
- [REQ-FR-SESSION-15] deriveMessages 投影 MUST：contentGeneration 变化时从该节点起整表重建，纯尾增长走增量投影；空 assistant（无内容块仅承载 max-tokens usage）MUST 投影为 null（dsh session/src/index.ts:984-1005、agent-loop/src/agent.ts:686-694）。
- [REQ-FR-SESSION-16] request/header canonical MUST：canonicalHeader 的 tools 为空时整体省略；tool schema 以 JSON.stringify 形态做顺序敏感比较；工具增删仅当 baseline 存在且名字集合有差才追加 developer/message，headerSeq 仅 additions>0 时携带（dsh agent-loop/src/agent.ts:635-665、request-header.ts:21-52）。
- [REQ-FR-SESSION-17] 继承切点 MUST 为最后一个 tagged session/end-seed 所在 seq：isSeeded 头必须携带继承事件数（cut），非 isSeeded 头 cut 必须为 0（dsh session-persistence-jsonl/src/format.ts:117-127）。
## Scenarios

Rule: REQ-FR-SESSION-1 — v4 事件全集与 EpochHeader

场景: v4 会话写入与读取回环
  假设 一个含 turn/step/消息/工具事件的会话被持久化为 session.v4.jsonl
  当 读取该文件重建会话时
  那么 全部事件逐条还原，无丢失与变形

场景: request/header reason 枚举
  假设 同一会话经历新建、续用、配置变化与请求序列中段
  当 各情形生成 request/header 时
  那么 reason 分别为 initial、resume、change、series

Rule: REQ-FR-SESSION-2 — v4 文件与写路径

场景: 非 v4 文件被拒绝
  假设 存在一个 version 不等于 4 的会话文件
  当 尝试加载时
  那么 返回明确错误且不产生任何写入

场景: zstd 压缩回环
  假设 启用 zstd 压缩
  当 写出并重新加载 session.v4.jsonl.zstd 时
  那么 事件序列逐条一致

场景: 写路径校验失败不发布后继文件
  假设 写路径中编码或校验失败
  当 走写路径时
  那么 不产生后继文件且已发布文件保持不变

Rule: REQ-FR-SESSION-3 — surface 与 replace-op

场景: replace-op 后源事件永久留存
  假设 压缩以 op 为 replace、startSeq 与 endSeq 界定的区间结算
  当 重放读取该区间时
  那么 区间内原事件全部留存，表面显示引用 sourceEventSeqs 的新内容

Rule: REQ-FR-SESSION-4 — deriveMessages 确定性

场景: deriveMessages 两次重建逐字节一致
  假设 同一份会话日志
  当 两次执行 deriveMessages 时
  那么 冻结请求逐字节一致

场景: ptc-dispatch 子调用不进请求
  假设 日志含 tool/ptc-dispatch-start 与 tool/ptc-dispatch 事件
  当 deriveMessages 重建时
  那么 重建结果不含任何子调用消息

Rule: REQ-FR-SESSION-5 — fork 与续用拒绝

场景: fork 会话拒绝被续用
  假设 存在一个由 session/fork 派生的会话
  当 以 --session-id 采纳该会话时
  那么 拒绝并报明确错误

Rule: REQ-FR-SESSION-6 — abort 补偿

场景: abort 补合成 tool/result
  假设 流中断时存在未派发的工具调用
  当 abort 结算时
  那么 每个未派发调用补一条合成错误 tool/result，重放合法

Rule: REQ-FR-SESSION-7 — 未知必需事件拒绝

场景: 未知必需事件拒绝重建
  假设 日志被注入一个非 ignorable 的未知 type 事件
  当 重建时
  那么 拒绝并报明确错误

Rule: REQ-FR-SESSION-8 — SessionHeader 字段

场景: SessionHeader 逻辑九字段与磁盘六必需键
  假设 新建一个会话
  当 写出 v4 文件时
  那么 首行头恰含六必需键 type/version/id/createdAt/isSeeded/delegationDepth，逻辑头九字段齐备且 origin 仅接受 'subagent'

场景: 携带 seedLength 的头被拒绝
  假设 一个 v4 头含未知字段 seedLength
  当 加载或重建时
  那么 拒绝并报明确错误

Rule: REQ-FR-SESSION-9 — 角色与 ContentBlock

场景: 五角色与七种 ContentBlock 往返无损
  假设 会话含 System/Developer/User/Assistant/ToolResult 五种角色与 text/reasoning/image/file/tool-call/tool-addition/tool-removal 七种 block
  当 写出并重载时
  那么 角色判别与 block 类型逐条还原

Rule: REQ-FR-SESSION-10 — system surface 路由

场景: in-history 增量追加
  假设 同一请求世代内提示词增量变化
  当 提交 surface 时
  那么 以 in-history 增量追加节点，不重写既有节点

场景: 世代更替走 first-node 归一化
  假设 请求世代更替
  当 提交 surface 时
  那么 system 节点按 first-node 归一化路由重写首节点

场景: 空渲染清空活跃尾部
  假设 system prompt 渲染结果为空
  当 提交 surface 时
  那么 活跃尾部 system 节点被清空且 head 节点以 replace 落为空文本节点

Rule: REQ-FR-SESSION-1 — 扩展事件与 end-seed 回环

场景: 扩展事件与 end-seed 参与回环
  假设 会话含 approval 扩展事件、tool/ptc 扩展事件与 session/end-seed
  当 写出并重载时
  那么 全部事件逐条还原

Rule: REQ-FR-SESSION-1 — 会话标题生成

场景: LLM 标题参数生效
  假设 配置了 LLM 且会话需要标题
  当 生成标题时
  那么 请求受 targetWords 5（CJK 10）、maxInput 4096 字节与 60s 超时约束

场景: 无 LLM 回退标题
  假设 无可用 LLM
  当 生成标题时
  那么 回退 5 词或 40 字节，上限 80 字节

Rule: REQ-FR-SESSION-11 — 编码与发布不变量

场景: 独占发布与回滚
  假设 新会话首发与一次 append 失败
  当 观察发布路径时
  那么 首发走临时文件加 link 独占发布且逐级目录 fsync，append 失败时文件 truncate 回滚无半行

场景: 文件名世代与头版本不一致报错
  假设 磁盘文件名为 v4 但头内 version 为 5
  当 加载时
  那么 报错拒绝

Rule: REQ-FR-SESSION-12 — surface 硬约束

场景: 非法 replace 被拒
  假设 replace 的 sourceEventSeqs 遗漏某个被遮蔽节点或 assistant 消息携带它
  当 提交时
  那么 校验失败拒绝落盘

场景: tool/result 替换越权
  假设 对某 tool/result 的 replace 同时改动 content 以外字段
  当 提交时
  那么 拒绝

Rule: REQ-FR-SESSION-13 — 两级补偿精确形态

场景: 两级补偿错误码与文案
  假设 一次 abort 同时产生调度器跳过与步级未启动调用
  当 恢复落盘时
  那么 前者为 ABORTED_BEFORE_DISPATCH 加 AbortError，后者为 TOOL_NOT_STARTED 或 TOOL_OUTCOME_UNKNOWN 精确文案，seq 连续且 time 复用最后真实事件

Rule: REQ-FR-SESSION-14 — 序列折叠编码

场景: 折叠编码回环
  假设 sourceEventSeqs 含长度 3 以上的连续段
  当 落盘并读回时
  那么 磁盘为折叠区间而内存还原为展开序列

Rule: REQ-FR-SESSION-1 — 事件全集枚举（枚举生成）

场景: 事件 turn/start 触发
  假设 会话运行中满足条件：开启 turn 时
  当 结算时
  那么 日志追加 turn/start 事件且字段与 dsh types.ts 逐字一致

场景: 事件 turn/start 表面归属
  假设 事件 turn/start 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 turn/start 回环
  假设 含 turn/start 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 turn/end 触发
  假设 会话运行中满足条件：turn 结束时携带 reason 枚举
  当 结算时
  那么 日志追加 turn/end 事件且字段与 dsh types.ts 逐字一致

场景: 事件 turn/end 表面归属
  假设 事件 turn/end 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 turn/end 回环
  假设 含 turn/end 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 step/start 触发
  假设 会话运行中满足条件：step 开启时
  当 结算时
  那么 日志追加 step/start 事件且字段与 dsh types.ts 逐字一致

场景: 事件 step/start 表面归属
  假设 事件 step/start 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 step/start 回环
  假设 含 step/start 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 step/end 触发
  假设 会话运行中满足条件：step 收尾时
  当 结算时
  那么 日志追加 step/end 事件且字段与 dsh types.ts 逐字一致

场景: 事件 step/end 表面归属
  假设 事件 step/end 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 step/end 回环
  假设 含 step/end 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 user/message 触发
  假设 会话运行中满足条件：用户输入提交时
  当 结算时
  那么 日志追加 user/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 user/message 表面归属
  假设 事件 user/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 user/message 回环
  假设 含 user/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 developer/message 触发
  假设 会话运行中满足条件：工具增删声明时
  当 结算时
  那么 日志追加 developer/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 developer/message 表面归属
  假设 事件 developer/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 developer/message 回环
  假设 含 developer/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 system/message 触发
  假设 会话运行中满足条件：系统提示词入表面时
  当 结算时
  那么 日志追加 system/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 system/message 表面归属
  假设 事件 system/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 system/message 回环
  假设 含 system/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 assistant/message 触发
  假设 会话运行中满足条件：成功结算时内嵌完整 stream
  当 结算时
  那么 日志追加 assistant/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 assistant/message 表面归属
  假设 事件 assistant/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 assistant/message 回环
  假设 含 assistant/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 assistant/attempt 触发
  假设 会话运行中满足条件：失败尝试时且不进模型历史
  当 结算时
  那么 日志追加 assistant/attempt 事件且字段与 dsh types.ts 逐字一致

场景: 事件 assistant/attempt 表面归属
  假设 事件 assistant/attempt 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 assistant/attempt 回环
  假设 含 assistant/attempt 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/call 触发
  假设 会话运行中满足条件：派发前追加且参数原始未解析
  当 结算时
  那么 日志追加 tool/call 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/call 表面归属
  假设 事件 tool/call 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/call 回环
  假设 含 tool/call 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/result 触发
  假设 会话运行中满足条件：finalize 后按模型序提交且 cite callSeq
  当 结算时
  那么 日志追加 tool/result 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/result 表面归属
  假设 事件 tool/result 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/result 回环
  假设 含 tool/result 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 request/header 触发
  假设 会话运行中满足条件：世代判定满足时
  当 结算时
  那么 日志追加 request/header 事件且字段与 dsh types.ts 逐字一致

场景: 事件 request/header 表面归属
  假设 事件 request/header 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 request/header 回环
  假设 含 request/header 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 request/context 触发
  假设 会话运行中满足条件：四字段变化时
  当 结算时
  那么 日志追加 request/context 事件且字段与 dsh types.ts 逐字一致

场景: 事件 request/context 表面归属
  假设 事件 request/context 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 request/context 回环
  假设 含 request/context 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 session/end-seed 触发
  假设 会话运行中满足条件：继承标记时
  当 结算时
  那么 日志追加 session/end-seed 事件且字段与 dsh types.ts 逐字一致

场景: 事件 session/end-seed 表面归属
  假设 事件 session/end-seed 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 session/end-seed 回环
  假设 含 session/end-seed 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 approval/policy 触发
  假设 会话运行中满足条件：策略切换时且 log-only
  当 结算时
  那么 日志追加 approval/policy 事件且字段与 dsh types.ts 逐字一致

场景: 事件 approval/policy 表面归属
  假设 事件 approval/policy 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 approval/policy 回环
  假设 含 approval/policy 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 approval/asked 触发
  假设 会话运行中满足条件：ask 发起时与 decided 配对
  当 结算时
  那么 日志追加 approval/asked 事件且字段与 dsh types.ts 逐字一致

场景: 事件 approval/asked 表面归属
  假设 事件 approval/asked 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 approval/asked 回环
  假设 含 approval/asked 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 approval/decided 触发
  假设 会话运行中满足条件：应答结算时
  当 结算时
  那么 日志追加 approval/decided 事件且字段与 dsh types.ts 逐字一致

场景: 事件 approval/decided 表面归属
  假设 事件 approval/decided 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 approval/decided 回环
  假设 含 approval/decided 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/ptc-dispatch-start 触发
  假设 会话运行中满足条件：子调用获得槽位时
  当 结算时
  那么 日志追加 tool/ptc-dispatch-start 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/ptc-dispatch-start 表面归属
  假设 事件 tool/ptc-dispatch-start 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/ptc-dispatch-start 回环
  假设 含 tool/ptc-dispatch-start 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/ptc-dispatch 触发
  假设 会话运行中满足条件：子调用结算时配对
  当 结算时
  那么 日志追加 tool/ptc-dispatch 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/ptc-dispatch 表面归属
  假设 事件 tool/ptc-dispatch 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/ptc-dispatch 回环
  假设 含 tool/ptc-dispatch 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

Rule: REQ-FR-SESSION-15 — 投影细则

场景: contentGeneration 变化触发整表重建
  假设 某表面节点的 contentGeneration 发生变化
  当 deriveMessages 重建时
  那么 从该节点起整表重建而非尾增量投影

场景: 表面尾增长走增量投影
  假设 仅在日志尾部追加了新表面节点
  当 deriveMessages 重建时
  那么 复用上次投影仅增量追加新节点

场景: 空 assistant 投影为 null
  假设 某 assistant 消息无内容块仅承载 max-tokens usage
  当 deriveMessages 投影时
  那么 该 assistant 投影为 null

Rule: REQ-FR-SESSION-16 — canonical header 细则

场景: canonicalHeader 空 tools 省略
  假设 世代内工具集合为空
  当 生成 canonicalHeader 时
  那么 tools 字段整体省略

场景: tool schema 比较顺序敏感
  假设 两个同名工具的 schema JSON 键序不同
  当 headerEquals 比较时
  那么 判定为不相等且 reason 升级为 change

场景: 工具增删 developer 声明门控
  假设 工具集合相对 baseline 的名字集合无差异
  当 结算新请求世代时
  那么 不追加 developer/message 且 headerSeq 不携带；有名字集合差且 baseline 存在时追加 developer/message，headerSeq 仅 additions>0 时携带

Rule: REQ-FR-SESSION-11 — 撕裂与世代发布

场景: zstd 撕裂尾恢复
  假设 session.v4.jsonl.zstd 尾部记录被截断
  当 加载时
  那么 丢弃不完整尾记录、完整记录全部还原且会话可继续追加

场景: 后继世代 EEXIST 逐字节比对
  假设 发布后继世代时目标文件名已存在
  当 link 返回已存在时
  那么 与已有文件逐字节比对：一致则采纳，不一致则报错拒绝

Rule: REQ-FR-SESSION-12 — node-0 保护

场景: node-0 重写白名单
  假设 对 node-0 的 replace 携带非 system/message 类型的单节点
  当 提交时
  那么 拒绝；仅 system/message 单节点重写被接受

Rule: REQ-FR-SESSION-17 — end-seed 切点

场景: 继承切点取最后一个 tagged end-seed
  假设 会话含多个 session/end-seed 事件
  当 计算继承切点时
  那么 切点为最后一个 tagged session/end-seed 所在 seq 且 isSeeded 头携带该继承事件数

## Dependencies

None.

## Source Trace

- doc/prd.md#7.1: SessionEvent 判别联合，字段与 dsh types.ts 逐字一致；durable 事件全集表
- doc/prd.md#7.2: 只实现 v4；世代命名，永不改名/删除已发布文件；读到非 v4 报明确错误
- doc/prd.md#7.3: surface 与 replace-op；旧内容永久留在日志
- doc/prd.md#7.4: deriveMessages 冻结请求必须可逐字节重现——对照测试锚点
- doc/prd.md#7.5: fork 与子代理会话不可被续用
- doc/prd.md#7.6: abort 补合成 tool/result
- confidence: high（PRD 第 7 节字段级规格）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
