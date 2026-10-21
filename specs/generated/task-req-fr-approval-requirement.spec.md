spec: task
name: "权限审批服务"
tags: [requirements, generated-draft, approval, permissions]
satisfies: [REQ-FR-APPROVAL]
---

## Intent

审批链是安全基线：无应答者、沙箱不可用、依赖未声明一律拒绝，绝不静默放行（fail-closed）。契约细节以 dsh user-approval 源码为权威（PRD 已一手核实行号）。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- ApprovalPolicy MUST 为 'ask' | 'never'；effectivePolicy MUST 为 override??config??'ask' 优先级链；`never` 在 dispatch 前由审批服务自身判 rejected（防 listener 绕过），不产生 approval/asked 事件对（dsh user-approval/src/index.ts:139）。
- request() MUST 处于 open turn 内；审计对 approval/asked 与 approval/decided 必须 turn-enclosed。
- 应答链（UI answerer）缺席或失败 MUST fail-closed 'unavailable'；越权返回值一律归一化为 'unavailable'（dsh user-approval/src/index.ts:55 结果词表 ['allowed-once','rejected','cancelled','unavailable']、:283-291 兜底与归一化）；唯一授予值 'allowed-once'，无持久 allow-list。
- 策略 MUST 持久化为 approval/policy 事件（log-only 不进 transcript、最后一条生效）；setPolicy 同值 no-op；切换时注入 user 消息，文案逐字 `The approval policy changed from "x" to "y" (changed by the user).`（x/y 为实际策略值）；abort 竞速先到者以 cancelled 结算（dsh user-approval/src/index.ts 各处）。
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
  Test: pending_req_fr_approval_never
  假设 策略为 never
  当 任一工具请求审批时
  那么 自动拒绝且不产生 approval/asked 事件对

Scenario: 权限模式映射策略
  Test: pending_req_fr_approval_requirement
  假设 权限模式为 danger-full-access
  当 解析审批策略时
  那么 策略为 never；其余非 danger 模式下默认 ask

Scenario: open turn 之外请求被拒
  Test: pending_req_fr_approval_open_turn
  假设 当前无 open turn
  当 调用 request() 时
  那么 请求被拒，不产生 approval/asked 事件

Scenario: 无 answerer fail-closed
  Test: pending_req_fr_approval_answerer_fail_closed
  假设 headless 形态无 UI answerer
  当 工具请求审批时
  那么 结果为 unavailable，模型收到拒绝结果

Scenario: 词表外返回值归一化
  Test: pending_req_fr_approval_requirement
  假设 answerer 返回结果词表之外的值
  当 审批结算时
  那么 归一化为 unavailable

Scenario: allowed-once 一次性授予
  Test: pending_req_fr_approval_allowed_once
  假设 answerer 应答 allowed-once
  当 审批结算时
  那么 该次放行且不产生任何持久 allow-list

Scenario: 策略切换落事件并通知模型
  Test: pending_req_fr_approval_requirement
  假设 用户把策略切换为 never
  当 切换结算时
  那么 append approval/policy 事件（最后一条生效）且向模型注入 user-message 通知

Scenario: bash 沙箱升级走同一 seam
  Test: pending_req_fr_approval_bash_seam
  假设 bash 调用携带 sandbox_permissions 升级
  当 pre-execute 返回 ask 时
  那么 走 ApprovalService 同一 seam 结算

Scenario: rejected 与 cancelled 正常结算
  Test: pending_req_fr_approval_rejected_cancelled
  假设 answerer 分别应答 rejected 与 cancelled
  当 结算时
  那么 按词表原值结算，不归一化为 unavailable，不产生放行

Scenario: 正常 ask 产生 asked 与 decided 配对
  Test: pending_req_fr_approval_ask_asked_decided
  假设 open turn 内工具触发 ask 且获得应答
  当 结算时
  那么 依次产生 approval/asked 与 approval/decided 且二者均在 turn 内

Scenario: 审批请求 id 每次新鲜
  Test: pending_req_fr_approval_id
  假设 同一会话连续两次 ask
  当 生成 ApprovalRequestId 时
  那么 每次为全新 randomUUID 且不复用

Scenario: 同值切换 no-op
  Test: pending_req_fr_approval_no_op
  假设 当前策略已是 never
  当 setPolicy 再设 never 时
  那么 no-op：不落事件、不注入通知

Scenario: 切换文案逐字
  Test: pending_req_fr_approval_requirement
  假设 策略从 ask 切到 never
  当 注入通知时
  那么 user 消息文本恰为 The approval policy changed from "ask" to "never" (changed by the user).

Scenario: abort 竞速结算
  Test: pending_req_fr_approval_abort
  假设 审批等待中同时发生 abort 与应答
  当 竞速结算时
  那么 先到者胜出，先到 abort 则以 cancelled 结算

Scenario: effectivePolicy 优先级链
  Test: pending_req_fr_approval_effectivepolicy
  假设 同时存在 override 与 config 值
  当 解析生效策略时
  那么 override 优先于 config 优先于默认 ask

## Questions

- Source trace: doc/prd.md#11.1: ApprovalService 契约；fail-closed 行号 :55/:62/:139/:283-291 已一手核实, confidence: high
- Replace pending test selectors with real test names before lifecycle verification.
