spec: task
name: "权限审批服务"
tags: [requirements, generated-draft, approval, permissions]
satisfies: [REQ-FR-APPROVAL]
---

## Intent

审批链是安全基线：无应答者、沙箱不可用、依赖未声明一律拒绝，绝不静默放行（fail-closed）。契约细节以 dsh user-approval 源码为权威（PRD 已一手核实行号）。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- ApprovalPolicy MUST 为 'ask' | 'never'；effectivePolicy MUST 为 override??config??'ask' 优先级链；`never` 在 dispatch 前由审批服务自身判 rejected（防 listener 绕过），answerer 不被派发；asked/decided 审计对仍完整产生（2026-10-29 Q-C 勘误：dsh request() 无条件 append 审计对、never 判在 decide 内——dsh user-approval/src/index.ts:224-233,270-275）。
- request() MUST 处于 open turn 内；审计对 approval/asked 与 approval/decided 必须 turn-enclosed。
- 应答链（UI answerer）缺席或失败 MUST fail-closed 'unavailable'；越权返回值一律归一化为 'unavailable'（dsh user-approval/src/index.ts:55 结果词表 ['allowed-once','rejected','cancelled','unavailable']、:283-291 兜底与归一化）；唯一授予值 'allowed-once'，无持久 allow-list。
- 策略 MUST 持久化为 approval/policy 事件（log-only 不进 transcript、最后一条生效）；setPolicy 同值 no-op；切换时注入 user 消息，文案逐字 `The approval policy changed from "x" to "y" (changed by the user).`（x/y 为实际策略值）；竞速取消先到者胜出（2026-10-29 Q-E 裁决：审批应答链同步结算、AbortSignal 不跨 dylib wire，M1 竞速载体=工具管线 worker-vs-abort，先到 abort 的调用以取消性合成错误结果结算、后到结果弃置；dsh user-approval/src/index.ts:115-119 各处为 dsh 侧 signal 竞速形态）。
- 集成：tools/pre-execute waterfall 返回 {kind:'ask'} 交 ApprovalService serviceAsk；沙箱升级（如 bash 的 sandbox_permissions）走同一 seam。

## Boundaries

### Allowed Changes
- crates/fr-approval/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: never 策略自动拒绝
  Test: never_policy_rejects_before_any_dispatch
  假设 策略为 never
  当 任一工具请求审批时
  那么 自动拒绝（decided=rejected）且不派发 answerer；asked/decided 审计对仍完整产生

Scenario: 权限模式映射策略
  Test: derive_matches_the_table_and_keeps_a_still_matching_selection
  假设 权限模式为 danger-full-access
  当 解析审批策略时
  那么 策略为 never；其余非 danger 模式下默认 ask

Scenario: open turn 之外请求被拒
  Test: request_outside_an_open_turn_refuses_verbatim
  假设 当前无 open turn
  当 调用 request() 时
  那么 请求被拒，不产生 approval/asked 事件

Scenario: 无 answerer fail-closed
  Test: request_without_answerers_fails_closed_with_audit_pair
  假设 headless 形态无 UI answerer
  当 工具请求审批时
  那么 结果为 unavailable，模型收到拒绝结果

Scenario: 词表外返回值归一化
  Test: throwing_and_rogue_answerers_fail_the_question_closed
  假设 answerer 返回结果词表之外的值
  当 审批结算时
  那么 归一化为 unavailable

Scenario: allowed-once 一次性授予
  Test: answerer_chain_dispatches_in_registration_order
  假设 answerer 应答 allowed-once
  当 审批结算时
  那么 该次放行且不产生任何持久 allow-list

Scenario: 策略切换落事件并通知模型
  Test: set_policy_notices_the_change_verbatim
  假设 用户把策略切换为 never
  当 切换结算时
  那么 append approval/policy 事件（最后一条生效）且向模型注入 user-message 通知

Scenario: bash 沙箱升级走同一 seam
  Test: missing_channel_or_agent_fail_closed
  假设 bash 调用携带 sandbox_permissions 升级
  当 pre-execute 返回 ask 时
  那么 走 ApprovalService 同一 seam 结算

Scenario: rejected 与 cancelled 正常结算
  Test: rejected_and_cancelled_settle_verbatim
  假设 answerer 分别应答 rejected 与 cancelled
  当 结算时
  那么 按词表原值结算，不归一化为 unavailable，不产生放行

Scenario: 正常 ask 产生 asked 与 decided 配对
  Test: request_without_answerers_fails_closed_with_audit_pair
  假设 open turn 内工具触发 ask 且获得应答
  当 结算时
  那么 依次产生 approval/asked 与 approval/decided 且二者均在 turn 内

Scenario: 审批请求 id 每次新鲜
  Test: ask_ids_are_fresh_uuid_v4
  假设 同一会话连续两次 ask
  当 生成 ApprovalRequestId 时
  那么 每次为全新 randomUUID 且不复用

Scenario: 同值切换 no-op
  Test: set_policy_notices_the_change_verbatim
  假设 当前策略已是 never
  当 setPolicy 再设 never 时
  那么 no-op：不落事件、不注入通知

Scenario: 切换文案逐字
  Test: set_policy_notices_the_change_verbatim
  假设 策略从 ask 切到 never
  当 注入通知时
  那么 user 消息文本恰为 The approval policy changed from "ask" to "never" (changed by the user).

Scenario: 竞速取消由工具管线承载
  Test: caller_abort_selects_before_dispatch
  假设 工具调用等待审批或执行中同时发生调用方 abort 与完成
  当 竞速结算时
  那么 先到者胜出；先到 abort 的调用以取消性合成错误结果结算，后到的完成结果弃置

Scenario: effectivePolicy 优先级链
  Test: effective_policy_reads_override_then_config_then_ask
  假设 同时存在 override 与 config 值
  当 解析生效策略时
  那么 override 优先于 config 优先于默认 ask

## Questions

- Source trace: doc/prd.md#11.1: ApprovalService 契约；fail-closed 行号 :55/:62/:139/:283-291 已一手核实, confidence: high
- [x] Replace pending test selectors with real test names before lifecycle verification.（批 3 绑定，2026-10-29）
