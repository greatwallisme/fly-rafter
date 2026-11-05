spec: task
name: "Agent 主循环插件"
tags: [requirements, generated-draft, agent-loop, core]
satisfies: [REQ-FR-AGENT-LOOP]
---

## Intent

fly-rafter 是字面全插件系统：agent 主循环不是宿主内置，而是 dougong 插件。宿主对循环零感知，循环与基础设施只经服务缝与事件解耦。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- fr-agent-loop MUST 是 dougong 插件：inject llm/tools/system-prompt/session-persistence，provide `agent` 服务（send/steer/inject/followup 入 inbox）；每会话一个循环 fiber，置于该会话 realm。
- 控制流 MUST 对齐 PRD 8.2 七步：输入入 inbox → turn()（append turn/start）→ preStep（inbox.claim → system-prompt.assemble → agent/pre-step waterfall → 拒绝或空输入直接收尾 turn）→ step（agent/request waterfall 提出 LlmCallConfig → llm.prepare_call → system prompt 提交 surface → append user/message → 按需 request/header、developer/message、request/context → deriveMessages 冻结请求 → 发起流）→ 流结算（error/aborted 时 append assistant/attempt + agent/request-error waterfall 决定重试或抛错；正常 append assistant/message；finish.kind == 'max-tokens' 粘滞结束 step）→ 工具执行（按 executionMode 分组）→ 收尾（step/end → agent/turn-stopping serial → turn/end；队列有 pending 开新 turn；llm.prepare_call 在无适配器时容忍中间件路由）。
- 每次请求 MUST 从会话日志 deriveMessages 投影并冻结，不经内存数组直传。
- step 抛错时 MUST 补齐缺失 tool/result（7.6 语义）；适配器级重试策略由 provider 插件经 provider_retry_policy 声明。
- 会话内热路径（agent/assistant-stream chunk）MUST 使用批量事件分发；分发开销基准测试 MUST 纳入 M1 验收。
- request/header 发布判定 MUST：首条 initial/resume；此后仅 canonical header 变化才 change（tool schema 用 JSON.stringify 顺序敏感比较）、仅序列边界才 series；工具增删 developer/message 仅 baseline 存在且名字集合有差，headerSeq 仅 additions 大于 0 时携带（dsh agent.ts:635-665、request-header.ts:21-52）。
- 缓存友好路由 MUST：capable 模型同世代提示词增量 in-history 追加；runtime-context 以 user 消息快照追加、完整当前值跟随保留历史之后（清空文案逐字 'Current runtime context: none. Earlier runtime-context snapshots no longer apply.'）；runtime-context 快照每个变更恰好 admitted 一次，重试 MUST NOT 重复注入；组装确定性保证 prompt 前缀逐字节稳定（dsh runtime-context.ts:14-16,96-119、agent.ts:425-440）。
- inbox 语义 MUST：claim 一次性取走全部 next-step、next-turn 至多 1 条；splice 规范化、message id 跨队列唯一、每次变更加 agent/inbox/spliced（dsh inbox.ts:109-114,198-243）。
- 长任务维持 MUST：每 turn 换新 AbortController 使旧信号闩失效；max-tokens 粘滞不得被 completed 降级；turn-stopping 仅 turnEnds 已设且 nextStep 空时派发；NO_ADAPTER 容忍中间件路由；缺 provider/model 配置 MUST 直接报错（dsh agent.ts:154-161,319-397,573-610）。
- attempt 结算 MUST 三分支：流 aborted 且中断内容非空白 → assistant/message 且 interrupted:true；error/aborted → assistant/attempt 并交 agent/request-error waterfall 裁决重试或抛错；正常 → assistant/message；user/message 仅 firstAttempt 落盘，重试 MUST NOT 重复注入（dsh agent.ts:425-441,464-545）。
- turn/step 状态机 MUST：相位 idle/maintenance/running；pre-step reject 置 turnEnds={blocked}；step0 无可领取输入 → turn reason completed 且不发起模型调用；turnEnds 已设后的空步静默收尾（dsh agent.ts:154-161,298-397）。
- fr-agent-loop 形态 MUST（2026-10-27 Q79-A 裁决，PRD 决策 46）：单实例为每会话维持一个循环线程跑自然阻塞控制流；入站五入口（send/steer/inject/followup/cancel）= 宿主 call() 快速入队 + condvar 唤醒即返；出站 = dougong v3 require（请求响应面）+ emit（事件面）；插件线程 teardown 返回前 join（v3 UB 纪律）；dsh「每会话一个循环插件 fiber」按 19.2 条 24 勘误为循环线程驱动体。
- LLM 流消费 MUST（2026-10-27 Q80-B/Q84-B2 裁决）：provider 侧 per-generation FIFO 缓冲 + 极小通知事件 {session_id, generation_id}（乱序/重复/丢失无害，丢失由下次通知或步边界兜底补偿）；循环线程非阻塞 require('llm','drain') 批量拉取（次序由 provider 缓冲保证，绕开 emit 次序无承诺契约）。
- 服务方法快路径不变量 MUST（全仓纪律，Q79-A/Q84-B2 推论）：服务方法 = 内存操作级快路径，慢工作（网络流/长 I/O）= 插件自有 worker 线程 + 通知/拉取；on_event 处理器内禁止 require、禁止阻塞（只入信箱/置唤醒标志）；whenIdle 跨 ABI 重建 = agent/status 事件 + 查询型 require（禁止阻塞等 idle 的 require）；initiator 因果归因 = 载荷显式携带（dsh AsyncLocalStorage 面结构性消失，行为等价；PRD 决策 46）。
- 切片边界（2026-10-27 Q81-A/Q83-A 裁决）：本切片循环侧建 tools/system-prompt 消费端封装（fr-wire 编解码 + 类型），以测试桩驱动全控制流；真 crate 随 Phase 14/15 装配对接；fr-system-prompt 含 scripts/extract-prompts 抽取脚本为切片 8。
- 生命周期臂 MUST（2026-11-04 18b Q2A 裁决）：`'agent'` 服务增 METHOD_CREATE=10 / METHOD_RESUME=11 / METHOD_LIVE=12（no-create 存活探测）。create = 会话创建（session-persistence METHOD_SESSION_CREATE）+ 驱动线程 spawn（决策 46 形态；dsh agents.create 行为 1:1）；resume = 持久层 resume/restore（方法带 6/7）组合 + 驱动构造 + setup 发布（dsh agents.resume→resumeWith 行为 1:1，agent-loop/index.ts:843-935）；wire 方法号 fr 自有（dsh 为进程内函数调用无 wire 层）；STATUS 按需建邮箱语义不变，探测臂禁建箱（dsh agents.get 存活判定 index.ts:266-276 的 fr 等价面）。Allowed += crates/fr-wire/src/services.rs（跨插件契约常量单文件，Q61A 判例同形）。
- `agent/error` 事件面 MUST（2026-11-04 18b Q4B 裁决）：驱动边界失败 containment 处 emit `agent/error` `{sessionId, turn, step, error, seq}`（dsh agent.ts:244-249 throwError 1:1：running 相位取 turn/step，否则 lastTurn/0；seq = 失败时会话 seq，即消费方 atOffset）；失败保留至消费方由其后终结事件治愈（dsh headless index.ts:365-372 治愈契约的发射半）。
- 生命周期路由投影 MUST（2026-11-04 18b Q9A 裁决）：CREATE/RESUME 载荷增可选 `reasoningEffort`/`maxTokens`（dsh agentOptions 四字段 1:1 投影，request seed 双消费 dsh agent.ts:581-582；`maxTokens` 按 assertAgentOptions 校验正安全整数、文案逐字 'agent maxTokens must be a positive safe integer'）；CREATE/RESUME 回 `{sessionId}`（会话句柄面）。
- provider 重试执行 MUST（2026-11-05 18c Q1A 裁决；决策 47-2A 引：独立 fr-llm-retry 插件，dsh llm-retry/src/index.ts 259 行 1:1）：经内核 waterfall 监听 `agent/request-error`（descriptor listen+CPS 续延，STATUS_DEFERRED 拉 next 续延→on_next 收下游结果）；两段载体=监听器即决即返（`llm/retry` 先持久 1:1 dsh backoff 先行 append；链值 fr 自有扩展 `{"kind":"retry","delayMs":N}`）→驱动做可取消 park 等待（deadline|cancel 命令|shutdown 三源唤醒，其余命令留队）→中止=丢弃决策（`llm/retry` 留存、无 `llm/retry-started`=dsh 逐字节同）→到期=驱动补 append `llm/retry-started` `{retryId,turn,step,retry}`→重试 step。durable 面零偏差、取消即时。门语义 1:1：policy 缺/码不可重试/normal 超限=放行下游（恒等回包=untouched→in-process 兜底）；always=先审下游再无帽退避（下游 warn `'llm-retry: provider "X" always policy ignored a downstream recovery failure: %o'`）；providerRetryAfterMs 有限>0 直用（超 maxDelayMs：normal 放行/always 本地退避）；localDelay 指数+对称抖动（fr-llm local_delay 单源）。Allowed += crates/fr-llm-retry/**。
- 重试状态与事件权 MUST（2026-11-05 18c 裁决）：llmRetry=fr-llm-retry 自持 (session,turn,step,provider,policyKey) 键控表（fr-compaction 自持会话表判例；dsh sessionProjections 通用投影服务不建——消费方到场纪律），首见会话冷读折叠重建（`llm/retry` 事件数据自带 turn/step/provider/policyKey/retry/retryId 全字段=精确 dsh fold 语义含重启恢复；同 (retry,retryId) 幂等守卫 1:1）；事件类型所有权=激活期 persistence DECLARE_EVENT_TYPES 声明 `llm/retry`+`llm/retry-started`（Q34A 通道）；retryId=uuid v4（dsh RetryId 品牌串，同链复用）。

## Boundaries

### Allowed Changes
- crates/fr-agent-loop/**
- crates/fr-wire/src/services.rs（跨插件契约常量单文件，Q61A 判例同形）
- crates/fr-llm-retry/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: agent 服务四入口入 inbox
  Test: four_entries_queue_inputs_with_their_targets
  假设 会话循环运行中
  当 调用 agent 服务的 send、steer、inject 或 followup 时
  那么 输入进入 inbox 并唤醒 driver

Scenario: 每会话循环 fiber 置于会话 realm
  Test: pending_req_fr_agent_loop_fiber_realm
  假设 新会话启动
  当 循环 fiber 创建时
  那么 fiber 置于该会话 realm，循环实现仅存在于 fr-agent-loop dylib 工件，宿主代码不含循环实现

Scenario: 完整 turn 无工具调用
  Test: full_turn_commits_the_boundary_shape
  假设 headless 任务提交后模型返回纯文本
  当 turn 结束时
  那么 会话日志含 turn/start、step/start、user/message、assistant/message、step/end、turn/end 且 reason 为 completed

Scenario: 含工具调用的 turn
  Test: tool_round_settles_calls_and_completes_the_turn
  假设 模型请求调用 bash 工具
  当 工具结果回填后继续下一 step
  那么 会话日志含模型序的 tool/call 与 tool/result，且无更多 tool-call 时 turn 为 completed

Scenario: max-tokens 粘滞结束
  Test: max_tokens_finish_sticks_the_turn_reason
  假设 流结算 finish kind 为 max-tokens
  当 结算完成时
  那么 step 粘滞结束且 turn reason 为 max-tokens

Scenario: steer 输入排队开新 turn
  Test: steer_during_a_turn_feeds_the_next_step
  当 turn 进行中调用 steer 时
  那么 输入入 inbox，当前 turn 正常收尾后开启下一 turn

Scenario: 冻结请求拒绝内存改动
  Test: derive_messages_skips_empty_and_log_only_and_caches_tail_growth
  假设 deriveMessages 已产出冻结请求
  当 尝试在内存中修改请求内容时
  那么 修改被拒绝，请求仍与日志投影一致

Scenario: step 抛错补齐缺失 tool/result
  Test: tool_abort_records_synthetic_results_for_unstarted_calls
  假设 step 抛错时存在未结算的工具调用
  当 失败恢复运行时
  那么 缺失的 tool/result 被补齐且重放合法

Scenario: provider 重试策略生效
  Test: provider_retry_retries_within_the_policy_and_throws_past_the_limit
  假设 provider 插件声明了 provider_retry_policy 且独立 fr-llm-retry 插件监听 agent/request-error
  当 流错误发生时
  那么 按策略重试（llm/retry 先持久、等待后 llm/retry-started、step 内重试），超限后向循环抛错

Scenario: 重试等待被取消中断
  Test: cancel_during_the_retry_wait_skips_retry_started_and_aborts
  假设 一次失败的 attempt 获得带延迟的重试决策且 llm/retry 已持久
  当 等待期间取消到达
  那么 决策被丢弃、不落 llm/retry-started、attempt 走中止结算路径（dsh backoff 可取消等待逐字节同）

Scenario: assistant-stream 批量分发基准
  Test: stream_dispatch_meets_the_m1_benchmark_floor
  假设 会话内 assistant-stream chunk 高频到达
  当 运行 M1 验收的分发开销基准时
  那么 chunk 经批量分发送达监听者且基准达标

Scenario: pre-step 拒绝或空输入收尾 turn
  Test: pre_step_reject_ends_the_turn_blocked
  假设 agent/pre-step waterfall 拒绝输入或 inbox 无可领取输入
  当 preStep 结算时
  那么 不发起请求，turn 直接收尾并 append turn/end

Scenario: turn 收尾前派发 turn-stopping
  Test: turn_stopping_dispatches_before_the_end_event
  假设 turn 即将正常结束
  当 收尾时
  那么 先以 serial 语义派发 agent/turn-stopping，再 append turn/end

Scenario: 无适配器时中间件路由容忍
  Test: no_adapter_prepare_call_falls_back_to_the_proposal
  假设 agent/request waterfall 提出的配置无绑定适配器
  当 调用 prepare_call 时
  那么 容忍中间件路由不报错，由后续件决定行为

Scenario: 多输入按入队顺序 claim
  Test: claim_takes_the_step_batch_then_one_turn_message
  假设 inbox 中排队多个输入
  当 preStep claim 时
  那么 按入队顺序消费

Scenario: incapable 模型走归一化路由
  Test: normalization_empties_tail_and_rewrites_head
  假设 当前模型不支持提示词缓存增量
  当 提交 system prompt surface 时
  那么 走归一化路由整段重写首节点

Scenario: header reason 判定
  Test: pending_req_fr_agent_loop_header_reason
  假设 会话经历新建、工具集变更、仅序列推进三种情形
  当 发布 request/header 时
  那么 reason 分别为 initial、change、series；工具 schema 仅顺序不同的相等比较按顺序敏感判定

Scenario: 策略变化不打破前缀
  Test: capable_series_appends_changed_text
  假设 会话中途审批策略变化
  当 构造下一请求时
  那么 历史前缀逐字节不变，新 runtime-context 快照追加在保留历史之后

Scenario: claim 队列语义
  Test: claim_takes_the_step_batch_then_one_turn_message
  假设 inbox 中有两条 next-step 与一条 next-turn 输入
  当 preStep claim 时
  那么 next-step 全部取走、next-turn 至多取一条，且变更产生 inbox/spliced 事件

Scenario: 新 turn 旧信号失效
  Test: waking_input_after_abort_starts_the_next_turn
  假设 上一 turn 的 AbortController 已触发
  当 开启新 turn 时
  那么 换新 controller，旧信号上的闩不再影响新 turn

Scenario: 中断保留部分内容
  Test: cancel_mid_stream_settles_the_interrupted_prefix
  假设 流 aborted 时已积累非空白文本与 reasoning 内容
  当 结算该 attempt 时
  那么 追加 assistant/message 且 interrupted:true，中断内容进入历史

Scenario: 重试不重复 user/message
  Test: retry_ruling_keeps_one_user_message
  假设 首次 attempt 失败进入重试
  当 第二次 attempt 发起请求时
  那么 user/message 仅存在 firstAttempt 的一条，未重复注入

Scenario: 错误 attempt 走 waterfall 裁决
  Test: error_attempt_settles_and_reports_the_failure
  假设 流以 error 结束
  当 结算时
  那么 追加 assistant/attempt 并交 agent/request-error waterfall 裁决，默认不重试则抛错

Scenario: step0 空输入不花模型调用
  Test: step0_empty_turn_completes_without_model_calls
  假设 turn 开启后 preStep 无可领取输入
  当 step0 结算时
  那么 turn reason 为 completed 且未发起任何模型请求

Scenario: pre-step reject 置 blocked
  Test: pre_step_reject_ends_the_turn_blocked
  假设 agent/pre-step waterfall 以 reject 裁决
  当 结算时
  那么 turnEnds 置为 blocked 并收尾 turn

Scenario: 维护相位不接受新步
  Test: maintenance_refuses_active_work_and_replays_a_latched_wake
  假设 循环处于 maintenance 相位
  当 请求发起新 step 时
  那么 该请求不被执行，循环仅在恢复 running 后继续

Scenario: runtime-context 恰好 admitted 一次
  Test: observe_follows_events
  假设 某步因流错误重试
  当 重试请求组装时
  那么 本步已注入的 runtime-context 快照不重复出现

Scenario: create 臂建会话并 spawn 驱动
  Test: create_arm_spawns_a_session_driver
  假设 组合内 session-persistence 与 fr-agent-loop 均已激活
  当 以 `{sessionId, cwd, provider?, model?, reasoningEffort?, maxTokens?}` 调用 agent 服务 METHOD_CREATE 时
  那么 持久层创建该会话、驱动线程 spawn 并回含会话句柄面

Scenario: resume 臂接续持久化会话
  Test: resume_arm_adopts_the_persisted_session
  假设 存在一个已持久化会话
  当 以该 id 调用 METHOD_RESUME 时
  那么 持久层 resume/restore 组合后驱动构造并发布 setup，会话历史可续

Scenario: live 探测不建邮箱
  Test: live_probe_reports_without_creating_mailboxes
  假设 进程内无该会话驱动
  当 以该 id 调用 METHOD_LIVE 时
  那么 回报非存活且会话邮箱表不新增条目

Scenario: 驱动边界失败发 agent/error
  Test: driver_boundary_failure_emits_agent_error
  假设 turn 循环内错误逸出到驱动边界
  当 containment 记录失败时
  那么 emit `agent/error` 载荷含 sessionId、turn、step、error、seq，running 相位取当前 turn/step、否则 lastTurn/0

## Questions

- Source trace: doc/prd.md#8.1: agent 主循环本身是插件；provide agent 服务（send/steer/inject/followup 入 inbox）, doc/prd.md#8.2: 控制流与 dsh agent-loop/src/agent.ts 逐步对齐（七步）, doc/prd.md#8.3: 失败恢复；provider_retry_policy, confidence: high（PRD 第 8 节流程级规格）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
