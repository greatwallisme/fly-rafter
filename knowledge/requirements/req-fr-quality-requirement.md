---
kind: requirement
id: REQ-FR-QUALITY
title: "质量门与命名硬约束"
status: accepted
liveness: auto
tags: [quality, naming, ci]
---

## Problem

项目质量门零容忍：fmt / clippy / test / doc 四关全绿；命名遵循 Rust 风格硬约束；1:1 对齐以对照测试锚定。

## Requirements

- [REQ-FR-QUALITY-1] 质量门 MUST 四关全绿：`cargo fmt --check`、`cargo clippy -- -D warnings`（lint 配置与 dougong workspace 对齐：rust lints + clippy all/pedantic/nursery/cargo 及选定 restriction 项）、`cargo test`、`cargo doc`（无警告）。
- [REQ-FR-QUALITY-2] 工具链 MUST 与 dougong 同步钉死（1.99.1 补丁级，含 wasm32-wasip2 与 musl target）。
- [REQ-FR-QUALITY-3] 命名 MUST：Rust 标识符 snake_case/UpperCamelCase/SCREAMING_SNAKE_CASE，禁 TS 风格 camelCase；自有用户可见命名 kebab-case（CLI flag、配置键、crate 名）；环境变量 SCREAMING_SNAKE（FR_ 前缀）；dsh 协议 wire 层（会话事件、工具 schema、SDK/ACP 参数）豁免保持逐字；dsh camelCase 插件配置键转 kebab-case（serde rename 对接）。
- [REQ-FR-QUALITY-4] 对照测试 MUST：golden session 回放——构造 v4 会话文件，断言 deriveMessages 重建的冻结请求与 dsh 同场景一致；`--json` 事件流与 SDK/ACP 帧各有 wire 一致性测试；同一任务在同模型同参数下，fly-rafter 与 dsh 产出的会话日志 MUST 逐事件同型（工具名/事件类型/字段名 diff 为空）。
- [REQ-FR-QUALITY-5] 文档即生成物 MUST：tool-catalog / config-catalog 等价物由脚本从真实装配生成并校验，防文档漂移。
- [REQ-FR-QUALITY-6] MUST NOT 以放宽 lint 或断言换取绿灯；测试用例以暴露缺陷为目标。
- [REQ-FR-QUALITY-7] 服务缝名与事件名 MUST 与 dsh 逐字一致——服务缝：llm、tools、fs、shell、terminals、subprocess、sandbox、web、lsp、jobs、subagents、goals、session-title、session-persistence、spill-store、mcp-resources；事件：agent/pre-step、agent/request、agent/request-error、agent/assistant-stream、agent/turn-stopping、tools/pre-execute、tools/execute、tools/post-execute。
- [REQ-FR-QUALITY-8] Invoker 消费 MUST 统一经 `ctx.require::<Invoker>`，MUST NOT 使用 `ctx.get`；跨 dylib/wasm 边界的服务调用 MUST 一律走此通道。
- [REQ-FR-QUALITY-9] workspace MUST 按 PRD 4.5 布局（26 crate）；依赖方向 MUST：fr 只依赖 fr-config 与各入口插件，任何领域 crate MUST NOT 依赖入口 crate，全部插件 crate MUST 依赖 dougong（基座 0.3.0 crates.io 依赖，本机可叠 path 覆盖，内核改动随里程碑发版）。

## Scenarios

Rule: REQ-FR-QUALITY-1 — 四关全绿

场景: 质量门四关通过
  假设 任意提交前
  当 运行四关命令时
  那么 fmt、clippy、test、doc 全部零警告零错误

Rule: REQ-FR-QUALITY-2 — 工具链钉死

场景: 工具链与 dougong 一致
  当 读取 rust-toolchain.toml 并与 dougong 对比时
  那么 1.99.1 补丁级一致且含 wasm32-wasip2 与 musl target

场景: lints 配置与 dougong 对齐
  当 对比 workspace lints 配置与 dougong 的 [workspace.lints] 时
  那么 rust lints 与 clippy 组合一致且未放宽

Rule: REQ-FR-QUALITY-3 — 命名硬约束

场景: 配置键 kebab-case
  假设 新增自有配置键
  当 校验键名时
  那么 键名为 kebab-case，dsh 协议 wire 字段豁免保持原拼写

场景: Rust 标识符无 camelCase
  当 对全部 crate 运行标识符扫描与 clippy 时
  那么 零 camelCase 标识符，serde rename 对接的 wire 原值除外

Rule: REQ-FR-QUALITY-4 — 对照测试锚点

场景: golden session 回放一致
  假设 构造与 dsh 同场景的 v4 会话文件
  当 deriveMessages 重建冻结请求时
  那么 与 dsh 产出的冻结请求一致

场景: 会话日志逐事件同型
  假设 同一任务在同模型同参数下分别由 fly-rafter 与 dsh 执行
  当 比对双方会话日志时
  那么 工具名、事件类型与字段名 diff 为空

场景: wire 一致性三面
  当 分别运行 --json 事件流、SDK 帧、ACP 帧的 wire 一致性测试时
  那么 三者与 dsh 基准一致

Rule: REQ-FR-QUALITY-5 — 文档即生成物

场景: 生成式文档无漂移
  当 重新生成 tool-catalog 与 config-catalog 等价物时
  那么 与入库版本 diff 为空

Rule: REQ-FR-QUALITY-6 — 禁止放宽

场景: 放宽 lint 的变更被拒绝
  假设 变更引入 lint 放宽或断言弱化
  当 质量门运行时
  那么 门禁失败且不得合并

Rule: REQ-FR-QUALITY-7 — 缝与事件名逐字

场景: 缝名与事件名清单逐字
  当 列举全部服务缝与事件名时
  那么 与 PRD 4.4 清单逐字一致

Rule: REQ-FR-QUALITY-8 — Invoker 消费约束

场景: ctx.get 消费被禁
  当 扫描全部插件 crate 源码时
  那么 不存在以 ctx.get 消费 Invoker 的用法

Rule: REQ-FR-QUALITY-9 — 依赖方向

场景: 领域 crate 依赖入口即失败
  假设 某领域 crate 引用了入口 crate
  当 运行结构检查时
  那么 检查失败，门禁不通过

## Dependencies

None.

## Source Trace

- doc/prd.md#21: 质量门、测试组织、对照测试、文档即生成物
- doc/prd.md#5.1: 命名规范硬约束
- doc/prd.md#1.4: 设计原则 5（质量门零容忍）
- confidence: high

## Open Questions

None.
