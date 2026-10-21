---
kind: requirement
id: REQ-FR-SANDBOX
title: "沙箱策略"
status: accepted
liveness: auto
tags: [sandbox, security]
---

## Problem

沙箱是工具执行的安全边界。fly-rafter 需要与 dsh sandbox-policy 对齐的三预设策略与 fail-closed 执行体链。

## Requirements

- [REQ-FR-SANDBOX-1] 三预设 read-only | workspace-write | danger-full-access；默认两层并存：插件 schema 缺省 read-only（fail-safe），装配层覆盖为 `FR_PERMISSION_MODE ?? workspace-write`（对齐 dsh cordis.patch.yml:246 的 !!js 表达式）；审批策略联动公式 `mode === 'danger-full-access' ? 'never' : 'ask'`；workspace root = 进程启动 cwd（无会话 cwd 时的 fallback root 由配置提供）。
- [REQ-FR-SANDBOX-2] 每次请求 MUST 把解析后的策略文本写入 runtime-context 快照（完整当前值跟随保留历史之后，保缓存安全）；文本模板 MUST 逐字：read-only 为 `Current DSH file policy: read-only. Any available operation enforced by the DSH file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.`（fr 版将其中 DSH 字样按 1:1 文本治理规则处理）；workspace-write 为 `Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace: ${JSON.stringify(policy.workspaceRoot)}. Some platform temporary areas may also be writable.`（dsh sandbox-policy/src/index.ts:44-47）。
- [REQ-FR-SANDBOX-3] 执行体后端链 MUST：Linux bubblewrap → Landlock，macOS Seatbelt，Windows ACL restricted-token；不可用即 fail-closed（`sandbox-unavailable`）。
- [REQ-FR-SANDBOX-4] 文件系统 fence 与观察策略 MUST 对齐 dsh fs-sandbox / fs-observation-policy。

## Scenarios

Rule: REQ-FR-SANDBOX-1 — 三预设与覆盖

场景: 默认 workspace-write 与 root
  假设 未设置 FR_PERMISSION_MODE
  当 解析权限模式时
  那么 生效 workspace-write 且 workspace root 为进程启动 cwd

场景: read-only 与 danger-full-access 预设
  假设 FR_PERMISSION_MODE 分别为 read-only 与 danger-full-access
  当 解析时
  那么 沙箱模式对应生效且审批按公式 mode === 'danger-full-access' ? 'never' : 'ask' 联动

场景: 两层默认并存
  假设 未设置 FR_PERMISSION_MODE 且读取插件 schema 缺省
  当 分别取插件缺省与装配生效值时
  那么 插件 schema 缺省 read-only、装配层生效 workspace-write，两层并存均须复现

Rule: REQ-FR-SANDBOX-2 — 策略文本快照

场景: 解析后策略文本进入 runtime-context
  假设 任意请求准备就绪
  当 构造 runtime-context 快照时
  那么 解析后的策略文本写入快照，且完整当前值跟随保留历史之后

Rule: REQ-FR-SANDBOX-3 — 执行体后端链

场景: bubblewrap 缺失降级 Landlock
  假设 Linux 上 bubblewrap 不可用而 Landlock 可用
  当 工具需要沙箱执行时
  那么 降级 Landlock 执行且不报 sandbox-unavailable

场景: 后端全缺失 fail-closed
  假设 Linux 上 bubblewrap 与 Landlock 均不可用
  当 工具需要沙箱执行时
  那么 报 sandbox-unavailable，不静默放行

Rule: REQ-FR-SANDBOX-4 — 文件系统 fence

场景: workspace 外写入被拒
  假设 workspace-write 模式
  当 工具尝试写 workspace 外路径时
  那么 被文件系统 fence 拒绝，观察策略与 dsh fs-sandbox 及 fs-observation-policy 一致

Rule: REQ-FR-SANDBOX-3 — 平台后端选择

场景: 按平台解析执行体后端
  假设 分别在 linux、macos、windows 目标声明下解析
  当 选择沙箱执行体时
  那么 依次命中 bubblewrap/Landlock、Seatbelt、ACL restricted-token 链

Rule: REQ-FR-SANDBOX-4 — 观察策略对齐

场景: 观察策略与 dsh 对齐
  假设 工具读取 workspace 外路径
  当 按观察策略结算时
  那么 行为与 dsh fs-observation-policy 的对照断言一致

## Dependencies

- REQ-FR-CONFIG

## Source Trace

- doc/prd.md#11.2: 三预设表、FR_PERMISSION_MODE 覆盖、执行体后端链、fail-closed
- confidence: high

## Open Questions

None.
