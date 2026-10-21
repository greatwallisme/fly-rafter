---
kind: requirement
id: REQ-FR-SUBAGENT
title: "子代理系统"
status: accepted
liveness: auto
tags: [subagent, agents]
---

## Problem

复杂任务需要委派子代理。fly-rafter 提供进程内 spawn/fork 与 ACP 委派桥；明确不做 claude-code / codex 委派桥（用户裁决）。

## Requirements

- [REQ-FR-SUBAGENT-1] 进程内 spawn MUST：child id = randomUUID 会话 id（external = provider 铸造、跨父跨 provider 唯一）；重复 id → `subagent "X" already exists` 码 DUPLICATE_CHILD；subagent 工具即时返回 `{kind:'activation', subagentId}` 渲染 `started subagent <id>`；父 durable log 收 subagent/catalog 分块清单记录（childId/label/mode）；生命周期边为事件 subagent/start（{runId,provider,id,local}）与 subagent/end（+stopReason,lastAssistantMessage?）作用于委派父；spawn 不共享任何对话上下文（inheritsParentContext:false、agentPreset 能力 true）。
- [REQ-FR-SUBAGENT-2] 结果送达 MUST：manager 清理后以 user 消息投递（父空闲 queue、运行中 steer）；文案族逐字 `Background subagent <id> finished and will do no further work unless you send it more.` / `…finished. It cannot receive follow-up messages.` / `…was stopped before it finished.` / `…ran out of room…` / `…declined the task.` / `…failed before it finished.`，后接 `Its closing message:`（无则 `It left no closing message.`）与可选 `Structured result: <json>`；source {kind:'subagent-settled',form:'notice',summary,senderSessionId}；stop reasons completed/aborted/error/max-tokens/refusal；可续 provider 的子初始 prompt 追加返回指引（父 id 与 send_message 指令）。
- [REQ-FR-SUBAGENT-3] fork MUST：种子 = 父日志最后一个 turn/end 为止的均衡完成 turn 前缀（in-flight turn 排除、无完成 turn 则空），创建时一次捕获为子自有持久 transcript；子继承 preset 组合与委托策略；cwd 默认父生效目录；fork agentPreset:false、inheritsParentContext:true。
- [REQ-FR-SUBAGENT-4] interrupt_agent MUST：同步输出 `interrupt requested for agent <id>` 与 {accepted:true}，不等待；取消信号下目标可继续运行至观察点；未领取 inbox、Activation 与已发布后代保留；external 目标永久停止；local 目标 cancel({kind:'user'|'parent'},{keepInbox:true})；目标不存在为 accepted no-op；UNAUTHORIZED 判四情形：非活跃祖先、self-interrupt、他父子代、非活跃后代。
- [REQ-FR-SUBAGENT-5] send_message MUST：运行中目标下 step 准入（steer）、空闲目标开新 turn（queue/followup）；渲染 `message delivered to agent <id>` 返回 {messageId}（送达确认非答案）；child→parent 投递包裹 `Agent <id> sent a message: …` source {kind:'agent-message',form:'relay',senderSessionId}；缺席 direct child 从持久层冷续；非驻留 child 向父发送拒绝 `…cannot send to parent…` UNAUTHORIZED；external child → NOT_CONTINUABLE。
- [REQ-FR-SUBAGENT-6] list_agents MUST：scope children（默认，直接子代自持久 catalog、不加载 Agent）| descendants（递归前序、行含 parent 与 depth）；one-shot 与 external 子代不列出；诊断行 {kind:'diagnostic',reason:'corrupt'|'unsupported'|'unavailable'}；status running/inactive 自活跃注册表；空渲染 `(no subagents)`；行格式 `<id> [<status>] parent=<p> depth=<d> — <label>`。
- [REQ-FR-SUBAGENT-7] 容量与深度 MUST：per-root ActivationPool 经不间断可续父链全后代共享；超限 → `subagent limit reached (active child limit: N); wait for an existing child to finish or complete this work with the current agents`；子深度 = 父 + 1、持久头为单调下界（resume 不归零）；超帽 → `subagent depth D exceeds maxDepth M`；数字 maxDepth 需 provider depthLimit 能力否则装载失败 `provider "X" cannot enforce maxDepth (no depthLimit capability) — set maxDepth: 'provider-managed'…`；到帽 agent 获模型上下文 `Delegation depth for \`subagent\`: current D, maximum M, remaining levels 0. Do not call…`。
- [REQ-FR-SUBAGENT-8] preset 门控 MUST：请求 agentPreset 需能力且 !inheritsParentContext，否则 UNSUPPORTED_CAPABILITY "does not support independent agent preset selection"；缺 agent-presets 服务 → PRESET_UNAVAILABLE；新建 append agent-preset/selected 与 subagent/descriptor 事件；省略继承父 preset；独立选 preset 的子冷续需 preset 投影否则 NOT_RESUMABLE。
- [REQ-FR-SUBAGENT-9] 委托策略 MUST：子审批钉 'never'；父沙箱 override 与 Auto/Full-access preset 以 source:'delegation' 事件捕获；子获固定上下文 "You are a delegated subagent: your permission scope was fixed…"。
- [REQ-FR-SUBAGENT-10] subagent-acp 委派桥 MUST 把任务委派给外部 ACP agent（permission 默认 reject、disposeGraceMs 配置），与 fr-acp 共享 ACP 客户端代码；MUST NOT 实现 claude-code / codex 委派桥。

## Scenarios

Rule: REQ-FR-SUBAGENT-1 — spawn 生命周期

场景: spawn 子会话 realm 隔离
  假设 主会话 spawn 一个子代理
  当 子代理运行时
  那么 子 agent 与子会话置于子 realm，工具即时返回 started subagent <id> 形态的 activation，子会话不进入模型主历史

场景: catalog 与生命周期事件
  假设 子代理自创建到结束
  当 检查父日志与事件时
  那么 父日志含 subagent/catalog 分块清单，subagent/start 与 subagent/end 事件载荷含 runId/provider/id/local 与 stopReason

场景: 重复 id 拒绝
  假设 请求的 child id 已存在
  当 创建时
  那么 报 subagent "X" already exists 码 DUPLICATE_CHILD

Rule: REQ-FR-SUBAGENT-2 — 结果送达

场景: 完成通知文案族
  假设 子代理分别以 completed、refusal、failed 结束且带收尾消息
  当 送达父会话时
  那么 对应逐字文案族出现并接 Its closing message: 与内容，source 为 subagent-settled notice

场景: 无收尾消息与结构化结果
  假设 子代理未留消息但产出结构化结果
  当 送达时
  那么 出现 It left no closing message. 与 Structured result: <json>

场景: 送达时机
  假设 父会话分别处于运行中与空闲
  当 子代理结束送达时
  那么 运行中走 steer 队列、空闲走 queue 开新轮

Rule: REQ-FR-SUBAGENT-3 — fork 语义

场景: fork 前缀派生
  假设 主会话含三个完成 turn 与一个进行中 turn 时调用 subagent_fork
  当 子会话创建时
  那么 种子为最后 turn/end 为止的均衡前缀、in-flight 排除、delegationDepth 递增，子以后续冷续重放该 transcript 而非重新 fork

场景: fork 无完成 turn
  假设 父日志无任何完成 turn
  当 fork 时
  那么 种子为空且子会话照常创建

Rule: REQ-FR-SUBAGENT-4 — interrupt_agent

场景: 中止请求不等待
  假设 子代理运行中
  当 调用 interrupt_agent 时
  那么 立即返回 interrupt requested for agent <id> 与 accepted:true；目标可运行至观察点，未领取 inbox 与已发布后代保留

场景: 非法目标四情形
  假设 分别对不存在目标、自身、他父子代、非活跃后代调用
  当 结算时
  那么 不存在为 accepted no-op，其余三类报 UNAUTHORIZED

Rule: REQ-FR-SUBAGENT-5 — send_message

场景: 送达确认非答案
  假设 子代理运行中收到 send_message
  当 结算时
  那么 渲染 message delivered to agent <id> 并返回 messageId，消息于该子下 step 准入

场景: child 到 parent 中继
  假设 子代理向父发消息
  当 投递时
  那么 以 Agent <id> sent a message: … 包裹且 source 为 agent-message relay

场景: external 与缺席目标
  假设 对 external child 发消息、缺席 direct child 发消息
  当 结算时
  那么 external 报 NOT_CONTINUABLE，缺席者自持久层冷续后送达

Rule: REQ-FR-SUBAGENT-6 — list_agents

场景: 清单行格式与空态
  假设 存在直接子代与孙代、另有 one-shot 子代
  当 分别以 children 与 descendants scope 列举时
  那么 children 仅直接子代，descendants 前序含 parent 与 depth；one-shot 不列；空时渲染 (no subagents)

场景: 诊断行与状态
  假设 catalog 存在损坏记录且部分子代活跃
  当 列举时
  那么 产出 diagnostic 行（corrupt/unsupported/unavailable）且活跃行 status 为 running

Rule: REQ-FR-SUBAGENT-7 — 容量与深度

场景: 激活池超限
  假设 活跃子代达 maxActiveSubagents 上限后再次 spawn
  当 结算时
  那么 报 subagent limit reached (active child limit: N) 文案且建议等待或收敛

场景: 深度超帽
  假设 委派深度达 maxDepth 后继续委派
  当 结算时
  那么 报 subagent depth D exceeds maxDepth M；到帽 agent 收到逐字 Delegation depth 上下文

场景: maxDepth 能力门控
  假设 provider 无 depthLimit 能力而配置数字 maxDepth
  当 装载时
  那么 失败并报 cannot enforce maxDepth (no depthLimit capability) 指引 provider-managed

Rule: REQ-FR-SUBAGENT-8 — preset 门控

场景: 独立 preset 能力校验
  假设 fork（inheritsParentContext）请求自定义 agentPreset
  当 结算时
  那么 UNSUPPORTED_CAPABILITY does not support independent agent preset selection；spawn 路径合法放行并 append agent-preset/selected 与 subagent/descriptor

场景: 冷续投影要求
  假设 独立选 preset 的子代理进程消失后冷续
  当 恢复时
  那么 preset 投影存在则恢复，缺失报 NOT_RESUMABLE

Rule: REQ-FR-SUBAGENT-9 — 委托策略

场景: 子审批与沙箱委托
  假设 父会话携带沙箱 override 与 Full-access preset 委派子代理
  当 子会话组装时
  那么 子审批策略钉 never、委托事实以 delegation 来源事件留存、子获固定委派上下文文案

Rule: REQ-FR-SUBAGENT-10 — ACP 桥与排除

场景: ACP 桥端到端委派
  假设 外部 ACP agent 进程可用
  当 通过 subagent-acp 委派任务时
  那么 任务完成结果回流主会话且权限请求默认 reject

场景: 委派桥清单仅含 ACP
  当 列举全部委派桥时
  那么 仅存在 subagent-acp，无 claude-code 与 codex 桥

## Dependencies

- REQ-FR-SESSION
- REQ-FR-AGENT-LOOP
- REQ-FR-ACP

## Source Trace

- doc/prd.md#15: 进程内 spawn/fork、配套工具、ACP 委派桥、不做 claude-code/codex
- confidence: high

## Open Questions

None.
