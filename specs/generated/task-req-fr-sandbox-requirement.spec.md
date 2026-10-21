spec: task
name: "沙箱策略"
tags: [requirements, generated-draft, sandbox, security]
satisfies: [REQ-FR-SANDBOX]
---

## Intent

沙箱是工具执行的安全边界。fly-rafter 需要与 dsh sandbox-policy 对齐的三预设策略与 fail-closed 执行体链。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- 三预设 read-only | workspace-write | danger-full-access；默认两层并存：插件 schema 缺省 read-only（fail-safe），装配层覆盖为 `FR_PERMISSION_MODE ?? workspace-write`（对齐 dsh cordis.patch.yml:246 的 !!js 表达式）；审批策略联动公式 `mode === 'danger-full-access' ? 'never' : 'ask'`；workspace root = 进程启动 cwd（无会话 cwd 时的 fallback root 由配置提供）。
- 每次请求 MUST 把解析后的策略文本写入 runtime-context 快照（完整当前值跟随保留历史之后，保缓存安全）；文本模板 MUST 逐字：read-only 为 `Current DSH file policy: read-only. Any available operation enforced by the DSH file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.`（fr 版将其中 DSH 字样按 1:1 文本治理规则处理）；workspace-write 为 `Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace: ${JSON.stringify(policy.workspaceRoot)}. Some platform temporary areas may also be writable.`（dsh sandbox-policy/src/index.ts:44-47）。
- 执行体后端链 MUST：Linux bubblewrap → Landlock，macOS Seatbelt，Windows ACL restricted-token；不可用即 fail-closed（`sandbox-unavailable`）。
- 文件系统 fence 与观察策略 MUST 对齐 dsh fs-sandbox / fs-observation-policy。

## Boundaries

### Allowed Changes
- crates/fr-sandbox/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 默认 workspace-write 与 root
  Test: pending_req_fr_sandbox_workspace_write_root
  假设 未设置 FR_PERMISSION_MODE
  当 解析权限模式时
  那么 生效 workspace-write 且 workspace root 为进程启动 cwd

Scenario: read-only 与 danger-full-access 预设
  Test: pending_req_fr_sandbox_read_only_danger_full_access
  假设 FR_PERMISSION_MODE 分别为 read-only 与 danger-full-access
  当 解析时
  那么 沙箱模式对应生效且审批按公式 mode === 'danger-full-access' ? 'never' : 'ask' 联动

Scenario: 两层默认并存
  Test: pending_req_fr_sandbox_requirement
  假设 未设置 FR_PERMISSION_MODE 且读取插件 schema 缺省
  当 分别取插件缺省与装配生效值时
  那么 插件 schema 缺省 read-only、装配层生效 workspace-write，两层并存均须复现

Scenario: 解析后策略文本进入 runtime-context
  Test: pending_req_fr_sandbox_runtime_context
  假设 任意请求准备就绪
  当 构造 runtime-context 快照时
  那么 解析后的策略文本写入快照，且完整当前值跟随保留历史之后

Scenario: bubblewrap 缺失降级 Landlock
  Test: pending_req_fr_sandbox_bubblewrap_landlock
  假设 Linux 上 bubblewrap 不可用而 Landlock 可用
  当 工具需要沙箱执行时
  那么 降级 Landlock 执行且不报 sandbox-unavailable

Scenario: 后端全缺失 fail-closed
  Test: pending_req_fr_sandbox_fail_closed
  假设 Linux 上 bubblewrap 与 Landlock 均不可用
  当 工具需要沙箱执行时
  那么 报 sandbox-unavailable，不静默放行

Scenario: workspace 外写入被拒
  Test: pending_req_fr_sandbox_workspace
  假设 workspace-write 模式
  当 工具尝试写 workspace 外路径时
  那么 被文件系统 fence 拒绝，观察策略与 dsh fs-sandbox 及 fs-observation-policy 一致

Scenario: 按平台解析执行体后端
  Test: pending_req_fr_sandbox_requirement
  假设 分别在 linux、macos、windows 目标声明下解析
  当 选择沙箱执行体时
  那么 依次命中 bubblewrap/Landlock、Seatbelt、ACL restricted-token 链

Scenario: 观察策略与 dsh 对齐
  Test: pending_req_fr_sandbox_dsh
  假设 工具读取 workspace 外路径
  当 按观察策略结算时
  那么 行为与 dsh fs-observation-policy 的对照断言一致

## Questions

- Source trace: doc/prd.md#11.2: 三预设表、FR_PERMISSION_MODE 覆盖、执行体后端链、fail-closed, confidence: high
- Replace pending test selectors with real test names before lifecycle verification.
