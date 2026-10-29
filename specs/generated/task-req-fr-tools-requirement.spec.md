spec: task
name: "工具系统与调度"
tags: [requirements, generated-draft, tools, pipeline]
satisfies: [REQ-FR-TOOLS]
---

## Intent

工具调用是 agent 执行能力的核心。fly-rafter 需要与 dsh `tools.execute` 逐项对齐的注册、schema、管线与调度语义。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- ToolRegistry MUST 支持 register（名称、JSON Schema、executionMode（exclusive/parallel）、presentation 元数据、说明 section order 1000-3000）；MCP 工具以 raw JSON Schema 动态注册。
- 执行管线 MUST 按序：createExecution（参数 JSON 快照/冻结）→ tools/pre-execute waterfall（allow / deny / {kind:'ask'} 交 ApprovalService）→ guards（单调串，防重复调用风暴）→ tools/execute waterfall → 工具体 → tools/post-execute → append tool/call（模型序）+ tool/result（模型序提交）；guards 仅 pre-execute allow 时求值、跨 waterfall 单调不放宽（无 allow 回退）；guard 拒绝的调用仍 MUST 过 tools/post-execute；post-execute block → isError 结果附 feedback 文本、value/content 双替换抛 TypeError；finalize 冻结执行记录后才提交 tool/result（dsh tools/src/index.ts:1153-1162,724-732,1501-1547,1789-1828,1677-1722）。
- 调度 MUST：exclusive 调用为单调用屏障（组=该调用本身）；parallel 有界滚动池上限=agentLoop.config.maxParallelToolCalls 默认 10；批次启动前 lazy 重分类（注册表变更可产生新屏障，并行组在首个非 parallel 调用处断开）；concludeTurn 仅成功结果终结 turn、错误结果 turn 继续；abort 为未开始调用补合成错误结果（dsh agent-loop/src/tool-calls.ts:85-105,200-213、tools/src/index.ts:593）。
- schema 类型推断与呈现元模型 MUST 对齐 dsh schema.ts / presentation.ts。
- schema 子集白名单 MUST（type/oneOf/properties/required/additionalProperties/items/enum/const 加注释词，其余键拒绝）；object 类的 additionalProperties MUST 显式 boolean（缺省或非布尔拒绝）；推断规则（dsh TS 类型层，2026-10-29 Q-B 勘误：const 优先于 enum 优先于宽类型、integer 归 number、oneOf 并集、深度 16 上限回退 JsonValue 均为 dsh 编译期类型推断 InferValueAt 非运行时行为；fr 运行时等值=author 子集校验+迭代编译，无递归且不设运行时深度上限）；isConcurrencySafe 仅严格 true 才 parallel（否则 fail-closed exclusive）（dsh schema.ts:97-166,153-170,367-369、json-schema.ts:6-10）。

## Boundaries

### Allowed Changes
- crates/fr-tools/**
- crates/fr-guard-repeat-reminder/**
- crates/fr-guard-timeout/**
- crates/fr-agent-loop/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 工具注册元数据齐备
  Test: registered_metadata_is_complete
  当 注册任一内置工具时
  那么 名称、JSON Schema、executionMode、presentation 元数据与说明 section order 全部登记

Scenario: 管线事件顺序
  Test: pipeline_runs_stages_in_order
  当 任一工具执行时
  那么 顺序为 createExecution、pre-execute、guards、execute、post-execute、tool/call 加 tool/result

Scenario: pre-execute deny 短路执行
  Test: pre_execute_deny_short_circuits_body
  假设 tools/pre-execute waterfall 返回 deny
  当 该调用进入管线时
  那么 不执行工具体，直接产出错误 tool/result

Scenario: exclusive 串行屏障
  Test: exclusive_calls_serialize_in_model_order
  假设 模型一次请求多个 exclusive 工具调用
  当 执行时
  那么 按模型序逐个执行，无并发重叠

Scenario: parallel 有界滚动池
  Test: parallel_pool_bounds_in_flight
  假设 模型一次请求多个 parallel 工具调用
  当 执行时
  那么 并发数不超过有界池上限，全部完成后才结算 step

Scenario: concludeTurn 调用终结 turn
  Test: concluding_result_ends_the_turn
  假设 模型请求 concludeTurn 类调用
  当 其 tool/result 提交后
  那么 turn 终结且不再开启下一 step

Scenario: schema 类型推断与 dsh 一致
  Test: compiles_nested_containers_with_wire_key_order
  当 对相同参数对象运行类型推断时
  那么 推断产出与 dsh schema.ts 的结果一致

Scenario: 重复调用达阈值触发守卫
  Test: gentle_then_detailed_escalation
  假设 同一工具被重复调用达到提醒阈值
  当 守卫链结算时
  那么 按阈值档位注入提醒（阈值取自默认 [3, 5, 8]）

Scenario: exclusive 与 parallel 混合分组屏障
  Test: mixed_groups_break_at_exclusive
  假设 模型一次请求依次含 parallel 组、exclusive 组、parallel 组
  当 分组调度时
  那么 组间按模型序串行推进，exclusive 组内串行、parallel 组内有界并发

Scenario: presentation 推断与 dsh 一致
  Test: presentation_hooks_soft_wrap
  当 对相同工具运行呈现元数据推断时
  那么 结果与 dsh presentation.ts 一致

Scenario: 执行中参数快照不可变
  Test: arguments_snapshot_is_frozen
  假设 createExecution 已冻结参数快照
  当 执行期间原始参数对象被修改时
  那么 工具体看到的仍为快照值

Scenario: 超集键拒绝与深嵌套编译
  Test: superset_keys_reject_and_deep_compiles
  假设 工具 schema 含白名单外键、另一工具含 16 层以上嵌套
  当 注册时
  那么 超集键注册失败；深嵌套 schema 迭代编译无递归、不设运行时深度上限（栈安全，2026-10-29 Q-B 勘误：原「回退 JsonValue」系 dsh TS 类型层推断非运行时行为）

Scenario: additionalProperties 非显式布尔拒绝
  Test: rejects_non_boolean_additional_properties
  假设 object 类 schema 的 additionalProperties 缺省或为非布尔值
  当 注册时
  那么 拒绝；仅显式 true/false 被接受

Scenario: guards 仅 allow 时求值且单调
  Test: guards_skip_non_allow_and_stay_monotonic
  假设 pre-execute 裁决为 deny 或 ask
  当 该调用进入守卫阶段时
  那么 guards 不求值；allow 路径上守卫状态跨 waterfall 单调，不允许已触发档位回退

Scenario: guard 拒绝仍过 post-execute
  Test: guard_denials_still_run_post_execute
  假设 guards 拒绝了某调用
  当 结算管线时
  那么 该调用仍经过 tools/post-execute 后产出拒绝结果

Scenario: post-execute block 双替换抛 TypeError
  Test: post_execute_decisions_apply_verbatim
  假设 tools/post-execute 裁决同时替换 value 与 content
  当 结算时
  那么 抛 TypeError；单字段 block 替换产生 isError 结果并附 feedback 文本

Scenario: parallel 池上限默认 10
  Test: pool_window_defaults_to_ten
  假设 一批 parallel 调用超过 10 个且配置未覆盖 maxParallelToolCalls
  当 调度时
  那么 同时在飞不超过 10，完成即递补

Scenario: 注册表变更触发惰性重分类
  Test: reclassification_breaks_the_pool
  假设 并行组执行中某未启动调用的 executionMode 被注册表更新为 exclusive
  当 填池扫描到该调用时
  那么 并行组在此处断开，该调用按新屏障语义执行

Scenario: concludeTurn 错误结果不终结 turn
  Test: error_result_does_not_conclude_the_turn
  假设 concludeTurn 类调用产出错误结果
  当 其 tool/result 提交后
  那么 turn 不终结且继续下一 step；仅成功结果终结 turn

## Questions

- Source trace: doc/prd.md#9.1: 注册与 schema；MCP 工具 raw JSON Schema 动态注册, doc/prd.md#9.2: 执行管线伪码, doc/prd.md#9.3: 调度（exclusive/parallel、concludeTurn、abort 补偿）, confidence: high（PRD 第 9 节）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- [x] Replace pending test selectors with real test names before lifecycle verification.（批 3 绑定，2026-10-29）
