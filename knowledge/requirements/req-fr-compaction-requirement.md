---
kind: requirement
id: REQ-FR-COMPACTION
title: "上下文管理与压缩"
status: accepted
liveness: auto
tags: [compaction, context]
---

## Problem

长会话超出模型上下文窗口。fly-rafter 需要与 dsh compaction / compaction-basic 对齐的压缩契约：summarizer 提示词逐字、剪枝、图片卸载与 spill 溢出存储，经 surface replace-op 落地。

## Requirements

- [REQ-FR-COMPACTION-1] 压缩契约与默认后端 MUST 对齐 dsh compaction / compaction-basic；summarizer 提示词常量 COMPACTION_INSTRUCTION MUST 逐字引用 dsh summarizer.ts:32-147（9 个固定 ## 节名与合并非复制既有 <compacted-summary> 规则在内）。
- [REQ-FR-COMPACTION-2] 触发数学 MUST：thresholdTokens = floor(min(contextWindow × 0.8, messageBudget − 65536))；messageBudget = contextWindow − reservedCompletionTokens；reserved = routed requestHeader maxTokens ?? adapter defaultMaxTokens ?? 0；缺上下文窗 → 警告一次后跳过；自动触发挂 agent/pre-step，仅 measured totalTokens ≥ thresholdTokens 时触发且剪枝后复查；pre-step 压缩失败警告 `step compaction failed: <msg>; continuing the turn` 后继续 turn。
- [REQ-FR-COMPACTION-3] 摘要请求 MUST：消息组成为（表面节点 0 为 system/message 时的 system head）+ 影子区消息按表面序 + 会话工具（自 request header）+ 恰一条 user 消息全文为 COMPACTION_INSTRUCTION；摘要目标 = 配置 summarizationProvider/Model 对 > 最新 routed request header > agent options，全无报错 "no provider/model available for summarization: ..."；provider/model 必须成对设置；调用 maxTokens = config.maxTokens（默认 headroomTokens 65536）。
- [REQ-FR-COMPACTION-4] 摘要 fail-closed MUST：finish error/aborted → LlmError；max-tokens → 错误码 'MAX_TOKENS' 文案 "summarization truncated at the token cap (incomplete checkpoint)"；image 输出拒绝 UNSUPPORTED_CONTENT；空文本 → "summarization produced no text summary content"。
- [REQ-FR-COMPACTION-5] 替换消息 MUST 为 CHECKPOINT_PREAMBLE（"This is an automatically generated checkpoint condensing an earlier span..."）+ `<compacted-summary>` 包裹摘要 + 收尾标记；收缩门：框架后估算 MUST 小于影子区 tokens 否则 "summary is not smaller than the shadowed content (N estimated framed tokens >= M)"；摘要错误经 waterfall 'compaction/summary-error'（recover 默认 false）裁决，重试时重备输入。
- [REQ-FR-COMPACTION-6] 保留 MUST：retainRatio 0.16 → retainTokens = floor(messageBudget × 0.16)；retainRatio ≥ thresholdRatio 拒绝装载；resolved retainTokens ≥ thresholdTokens 拒绝装载；选区永不含 node-0 的 system/message（firstIdx=1）、自尾累积至 retainTokens、边界回退到 toolPairingBalancedBefore 使 assistant tool-call/result 对不拆分、无可压缩返回 null。
- [REQ-FR-COMPACTION-7] 溢出恢复 MUST：agent/request-error 码 CONTEXT_WINDOW_EXCEEDED → 剪枝 + retainTokens 0 强制压缩，成功 {kind:'retry'}；计数器在 idle 与每 assistant/message 重置；耗尽透传原错误；表面已收缩后压缩失败仍重试；自动压缩仍超限报 "compaction still above threshold after N compaction attempts (X estimated tokens >= threshold Y)"。
- [REQ-FR-COMPACTION-8] 压缩 MUST 以持久括弧落地：compaction/start … compaction/summary + replace-surface user/message … compaction/end；失败 close 留可检测未配对 start；摘要结算后 MUST 全表面稳定性复查。
- [REQ-FR-COMPACTION-9] 工具结果剪枝 MUST：仅触 tool/result 表面事件；度量=文本块 Unicode 码点（非文本块计 0）；total > 8192 才剪；移除段 = [4096, total−1024) 码点区间；标记仅在首个相交块插入一次，文本逐字 `\n\n[... tool result middle pruned ...]\n\n`；非文本块保序；替换结果须更小且 ≤ threshold；每次替换 append compaction/prune 影子事件 + surfaceOp replace 的 tool/result（sourceEventSeqs=[seq]）；配置校验 head+marker+tail ≤ threshold。
- [REQ-FR-COMPACTION-10] spill MUST：max-inline-tokens 12500（省略=禁用）；挂 tools/post-execute 的 accepted 结果（跳过 exec.name==='read' 与纯文本 PTC 子调用）并限界 PTC dispatch 日志；定价=文本估算 + 每图模型 adapter 图像定价；超限全文落盘（图片渲染为 `[Image: <readonlyPath>; <mediaType>; <w>x<h>. Use read_image to view it.]`）；模型面替换 = 保留头 + `\n\n[...]\n\n` + 保留尾 + footer `(Omitted N bytes.[ Omitted M images.] Full formatted result stored at: <locator>. <retrievalHint>)`；预算预留最坏 notice 后头尾对半、图片不可分、surrogate 安全切片；任一步失败警告 `spill-policy: ...; keeping the inline content` 保留内联；存储布局 `<root>/session-<sha256(sessionId) 前 12hex>/<12hex 随机>-<encodedName>`、文件 0600 目录 0700、缺省 root=OS temp 下 mkdtemp `dsh-spill-XXXXXX`；启动清扫删 mtime 超清理期（默认 30 天、0 禁用）并剪空目录。
- [REQ-FR-COMPACTION-11] /compact MUST：仅空闲 agent（busy → "manual compaction requires an idle agent with no waking queued work"）；选区 retainTokens 0；成功 → `Compacted N history items (~M tokens).`；无可压缩 → 'No compactable history yet.'；取消 → "Compaction cancelled."；错误码 busy/cancelled/changed/summary/commit/persistence 各有固定文案；落地前 ctx.sessions.flush 确保持久。

## Scenarios

Rule: REQ-FR-COMPACTION-1 — 契约与 summarizer 提示词

场景: summarizer 提示词逐字一致
  当 以脚本抽取 summarizer 提示词常量时
  那么 与 dsh summarizer.ts:32-147 逐字一致，含 9 个固定 ## 节名与合并非复制规则

场景: 摘要请求消息组成
  假设 压缩启动且表面节点 0 为 system/message
  当 构造摘要请求时
  那么 消息含 system head、影子区消息按表面序、会话工具与恰一条全文为指令的 user 消息

Rule: REQ-FR-COMPACTION-2 — 触发数学

场景: 阈值计算与触发
  假设 上下文窗 200000、routed maxTokens 8192
  当 计算 thresholdTokens 时
  那么 取 floor(min(200000×0.8, (200000−8192)−65536)) 且 measured totalTokens 达阈值即在 pre-step 触发

场景: 缺上下文窗跳过
  假设 adapter 未声明上下文窗
  当 解析目标压力时
  那么 警告一次后跳过该 provider/model，不报错中断

场景: 剪枝后复查
  假设 触发时先执行工具结果剪枝
  当 剪枝结算后
  那么 复查 totalTokens，已低于阈值则不再压缩

Rule: REQ-FR-COMPACTION-3 — 摘要目标与参数

场景: 目标三级回退
  假设 依次配置摘要对、缺省摘要对两种部署
  当 选摘要目标时
  那么 配置对优先于最新 routed header 再优先于 agent options，缺配对拒绝装载，全无报 no provider/model available for summarization

Rule: REQ-FR-COMPACTION-4 — fail-closed

场景: max-tokens 截断拒绝
  假设 摘要流以 max-tokens 结束
  当 结算时
  那么 错误码 MAX_TOKENS 文案 summarization truncated at the token cap (incomplete checkpoint)

场景: 空文本拒绝
  假设 摘要流成功但零文本
  当 结算时
  那么 报 summarization produced no text summary content

Rule: REQ-FR-COMPACTION-5 — 替换与收缩门

场景: 替换消息形状
  假设 摘要文本就绪
  当 构造替换 user 消息时
  那么 为 CHECKPOINT_PREAMBLE 加 <compacted-summary> 包裹与收尾标记

场景: 收缩门拒绝
  假设 框架后估算不小于影子区 tokens
  当 结算替换时
  那么 拒绝并报 summary is not smaller than the shadowed content (N estimated framed tokens >= M)

场景: summary-error waterfall 默认不恢复
  假设 摘要调用失败
  当 compaction/summary-error waterfall 裁决时
  那么 recover 默认 false；重试时重备输入

Rule: REQ-FR-COMPACTION-6 — 保留与选区

场景: 保留边界与配平回退
  假设 尾部累积越过 retainTokens 且下一节点是半个 tool 对
  当 选区结算时
  那么 边界回退到配平位置使 assistant tool-call/result 对不拆分，且永不含 node-0

场景: 非法保留配置拒绝装载
  假设 retainRatio ≥ thresholdRatio 或 resolved retainTokens ≥ thresholdTokens
  当 插件装载时
  那么 拒绝并报明确错误

Rule: REQ-FR-COMPACTION-7 — 溢出恢复

场景: 上下文超限恢复重试
  假设 请求失败码 CONTEXT_WINDOW_EXCEEDED
  当 恢复结算时
  那么 剪枝并以 retainTokens 0 强制压缩，成功返回 retry 且计数器随后续 assistant/message 重置

场景: 恢复耗尽透传
  假设 恢复重试次数耗尽仍失败
  当 结算时
  那么 透传原始请求错误

Rule: REQ-FR-COMPACTION-8 — 持久括弧

场景: 括弧事件序列
  假设 一次压缩成功
  当 检查日志时
  那么 含 compaction/start 与 compaction/summary、replace-surface 与 compaction/end 的完整序列，失败 close 留可检测未配对 start

场景: 全表面稳定性复查
  假设 摘要结算时表面已被并发修改
  当 复查时
  那么 检出变化并按稳定性规则处理（放弃或重试）

Rule: REQ-FR-COMPACTION-9 — 剪枝细则

场景: 剪枝度量与标记
  假设 某工具结果含 10000 码点文本与非文本块
  当 剪枝结算时
  那么 仅按文本码点计总量，超 8192 才剪，移除 [4096, total−1024) 段，标记一次逐字 \n\n[... tool result middle pruned ...]\n\n，非文本块保序

场景: 剪枝落盘形态
  假设 一次剪枝生效
  当 检查日志时
  那么 append compaction/prune 影子事件与 surfaceOp replace 的 tool/result 且 sourceEventSeqs 恰含原 seq

Rule: REQ-FR-COMPACTION-10 — spill

场景: spill 替换形状与 footer
  假设 某工具结果估算超 12500 tokens
  当 spill 结算时
  那么 全文落盘、内联替换为头加 [...] 加尾加逐字 footer (Omitted N bytes.[ Omitted M images.] Full formatted result stored at: <locator>. <retrievalHint>)，read 工具与纯文本 PTC 子调用豁免

场景: spill 存储与清扫
  假设 多会话产生 spill 文件且存在超 30 天旧文件
  当 启动清扫运行时
  那么 删除超期文件并剪空目录，路径为 session-<sha256 前 12hex> 布局、文件 0600 目录 0700

场景: spill 失败保留内联
  假设 spill 落盘或替换任一步失败
  当 结算时
  那么 警告 spill-policy: ...; keeping the inline content 并保留内联原文

Rule: REQ-FR-COMPACTION-11 — 手动压缩

场景: /compact 空闲限定与文案
  假设 agent 忙碌时执行 /compact
  当 结算时
  那么 报 manual compaction requires an idle agent with no waking queued work；空闲成功时回 Compacted N history items (~M tokens).，无历史回 No compactable history yet.

## Dependencies

- REQ-FR-SESSION
- REQ-FR-LLM

## Source Trace

- doc/prd.md#12: 契约与默认后端对齐 compaction/compaction-basic；summarizer.ts:33 逐字；剪枝参数；图片卸载；spill max-inline-tokens 12500；/compact 触发
- confidence: high

## Open Questions

None.
