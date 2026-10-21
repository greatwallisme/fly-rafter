spec: task
name: "范围排除与偏差治理"
tags: [requirements, generated-draft, scope, exclusions]
satisfies: [REQ-FR-SCOPE]
---

## Intent

PRD 第 19 节记录了用户确认的裁剪与有意偏差。为防止范围蔓延与静默偏差，以 MUST NOT 形式固化，并要求新偏差先记录后确认。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- MUST NOT 实现 PRD 19.1 排除清单全部条目：语音输入全家；Agent Teams 9 工具；浏览器/计算机使用 6 工具；workflow、ralph、present、create_worktree、webhook；hooks 桥（Claude Code / Codex hooks.json）；遥测（OTel、匿名用户 ID、dsh_session_log 回传）；HMR 热重载；插件市场 plugin_market；Electron 桌面端；PTC Python 后端；Python SDK 客户端；会话格式迁移链 v0→v3；子代理 claude-code / codex 委派桥。
- 有意偏差 MUST 限于 PRD 19.2 已记录清单（!!js 受限子集、品牌/命名替换、配置键 kebab-case，及 2026-10-21 源码核对产生的勘误与补遗记录条 5）；实现中发现新的不可避免偏差 MUST 先记录到 PRD 19.2 并经用户确认后才可继续。
- 与 DeepSeek 官方 API 交互 MUST 仅使用标准 Anthropic Messages 协议；MUST NOT 实现 dsh_session_log 回传、dsh_plugin_packages 上报与 x-deepseek-harness-* 头。
- dsh 源码漂移 MUST 视为显式决策：PRD 锚定 v0.2.1-rc.2，参照物路径固定。

## Boundaries

### Allowed Changes
- crates/**
- doc/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 排除项不出现于装配
  Test: pending_req_fr_scope_requirement
  假设 全部内置插件注册完成
  当 列举插件与工具名时
  那么 不含 PRD 19.1 排除清单任一名称

Scenario: 新偏差先记录后实施
  Test: pending_req_fr_scope_requirement
  假设 实现中发现新的不可避免偏差
  当 尚未记录到 PRD 19.2 并经用户确认时
  那么 该偏差不得落地

Scenario: 官方 API 交互无私有扩展
  Test: pending_req_fr_scope_api
  当 检查全部对外 HTTP 请求与代码时
  那么 无 dsh_session_log、dsh_plugin_packages 与 x-deepseek-harness 前缀头

Scenario: dsh 版本锚定
  Test: pending_req_fr_scope_dsh
  当 检查参照物路径与版本时
  那么 锚定 v0.2.1-rc.2 与固定路径，升级需显式决策

Scenario: 排除工具 ralph 不存在
  Test: pending_req_fr_scope_ralph
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 ralph

Scenario: 排除工具 workflow 不存在
  Test: pending_req_fr_scope_workflow
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 workflow

Scenario: 排除工具 team_task 不存在
  Test: pending_req_fr_scope_team_task
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 team_task

Scenario: 排除工具 spawn_teammate 不存在
  Test: pending_req_fr_scope_spawn_teammate
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 spawn_teammate

Scenario: 排除工具 wait_agent 不存在
  Test: pending_req_fr_scope_wait_agent
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 wait_agent

Scenario: 排除工具 stagehand 不存在
  Test: pending_req_fr_scope_stagehand
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 stagehand

Scenario: 排除工具 create_worktree 不存在
  Test: pending_req_fr_scope_create_worktree
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 create_worktree

Scenario: 排除工具 present 不存在
  Test: pending_req_fr_scope_present
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 present

Scenario: 排除工具 plugin_market 不存在
  Test: pending_req_fr_scope_plugin_market
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 plugin_market

## Questions

- Source trace: doc/prd.md#19.1: 排除清单（用户确认裁剪）, doc/prd.md#19.2: 有意偏差 3 条与治理规则, doc/prd.md#19.3: wire 层声明, doc/prd.md#22: dsh 源码漂移对策, confidence: high
- Replace pending test selectors with real test names before lifecycle verification.
