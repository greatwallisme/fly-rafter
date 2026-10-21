spec: task
name: "系统提示词组装"
tags: [requirements, generated-draft, prompt, system-prompt]
satisfies: [REQ-FR-PROMPT]
---

## Intent

提示词文本是 1:1 对齐的核心面。fly-rafter 需要有序 section/context 注册、{{var}} 插值与逐字文本保真；文本必须脚本抽取生成、禁止手抄。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- fr-system-prompt MUST 提供有序 PromptSection / Context 注册 + `{{var}}` 插值 + `tool-order`（对齐 dsh toolOrder）。
- SECTION_ORDERS MUST 以 dsh system-prompt/src/index.ts:131-165 为权威、CONTEXT_ORDERS 以 :170-179 为权威（勘误注记：CONTEXT_ORDERS 曾误记 :149，行号实为 :170、数值不变）；HARNESS_IDENTITY: -1000、TOOLS_SDK: 5000、STRUCTURED_OUTPUT: 9900；WORKING_DIRECTORY: 100、SANDBOX_POLICY: 110、APPROVAL_POLICY: 115。
- 身份句 MUST 为 `You are an AI agent powered by Fly Rafter.`；`include-harness-identity` 默认 true；`persona-prefix` 默认 `''`；除身份句外全部 section 文本逐字取自 dsh。
- AGENTS.md 注入 MUST 非 system section：渲染为 user 消息包裹 system-reminder 帧（首行 "Instructions from: <path>"）；变更/删除以 Updated/removed 语义再注入；fs 工具触碰嵌套指令文件触发再投影；候选 AGENTS.md/CLAUDE.md 加 local 变体、projectRoot 自 cwd 向上找 .git 标记、maxSourceBytes 1MiB 超限跳过、maxBytes 二分截断恰好适配、同组去重；各工具说明 section 由工具插件注册（order 1000-3000）（dsh context/agent-instructions/src/render.ts:255-272,315-321、config.ts,files.ts、index.ts:4-5）。
- 提示词常量 MUST 由脚本从 dsh 源文件抽取生成，禁止手抄。
- 组装确定性 MUST：同 order 按 name code-unit 序、重名抛错、scoped 遮蔽 global；complete section 至多一个、waterfall 后恢复唯一 prompt section、refreshContext 只重跑 context 节；toolOrder 保留标记 unlisted-tools 恰好出现一次、未列工具字典序插入 rest 位；变量名限小写首字母加数字下划线、畸形或未注册抛错并指明 section、interpolate:false 保留字面（dsh system-prompt/src/index.ts:196-239,248-250,290-377,582-602,653-656）。
- SECTION_ORDERS 全表 32 项数值 MUST 逐字为：HARNESS_IDENTITY -1000、DEPLOYMENT_PERSONA_PREFIX 0、PLAN_POLICY 500、TEAM_POLICY 600、PTC_ONLY 800、FILE_REFERENCE 900、TOOL_BASH 1000、TOOL_PWSH 1010、TOOL_READ 1100、TOOL_WRITE 1200、TOOL_EDIT 1300、TOOL_GLOB 1400、TOOL_GREP 1500、TOOL_JOBS 1600、TOOL_PTY 1700、TOOL_WEB_SEARCH 2000、TOOL_WEB_FETCH 2100、TOOL_LSP 2200、TOOL_SESSION_QUERY 2300、TOOL_GOAL 2400、TOOL_WORKFLOW 2600、TOOL_RALPH 2700、TOOL_SUBAGENT 2800、TOOL_REPORT 2900、TOOL_COMPUTER_USE 3000、MCP_SERVERS 3100、TOOLS_SDK 5000、DELIVERABLE_FILE_REFERENCES 9000、STRUCTURED_OUTPUT 9900、HARNESS_SOURCE 10000、WEB_SURFACE 10100、DEPLOYMENT_PERSONA_SUFFIX 10200；CONTEXT_ORDERS 7 项：WORKING_DIRECTORY 100、SANDBOX_POLICY 110、APPROVAL_POLICY 115、SUBAGENT_DELEGATION 120、TEAM_IDENTITY 125、HARNESS_SOURCE 200、WEB_SURFACE 210（dsh system-prompt/src/index.ts:131-165,170-179）。

## Boundaries

### Allowed Changes
- crates/fr-system-prompt/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 变量插值与 tool-order 重排
  Test: pending_req_fr_prompt_tool_order
  假设 section 文本含占位变量且注册工具集变化
  当 组装提示词时
  那么 占位变量替换为运行时值，工具说明 section 按当前工具集与 tool-order 重排

Scenario: section 全序与 dsh 源一致
  Test: pending_req_fr_prompt_section_dsh
  当 组装默认装配提示词时
  那么 section 顺序符合 SECTION_ORDERS 与 CONTEXT_ORDERS（HARNESS_IDENTITY 最前、TOOLS_SDK 位于 5000），全集与 dsh 源文件一致

Scenario: 身份句默认渲染
  Test: pending_req_fr_prompt_requirement
  假设 include-harness-identity 保持默认 true
  当 组装提示词时
  那么 section 序列含 You are an AI agent powered by Fly Rafter.

Scenario: 身份句可关闭与前缀
  Test: pending_req_fr_prompt_requirement
  假设 include-harness-identity 为 false 且 persona-prefix 非空
  当 组装提示词时
  那么 无身份句 section，前缀文本按序出现在原身份句位置

Scenario: 项目指令进入 agent-instructions
  Test: pending_req_fr_prompt_agent_instructions
  假设 工作目录存在 AGENTS.md
  当 组装提示词时
  那么 其内容进入 agent-instructions section

Scenario: 提示词常量与 dsh 源逐字一致
  Test: pending_req_fr_prompt_dsh
  当 以抽取脚本生成提示词常量并与 dsh 源文件比对时
  那么 除身份句外逐字一致

Scenario: plan-mode 文案与 base patch 逐字
  Test: pending_req_fr_prompt_plan_mode_base_patch
  假设 计划模式启用
  当 组装计划模式 section 时
  那么 文案与 dsh base patch 的 plan-mode 行逐字一致

Scenario: 无 AGENTS.md 不产生空 section
  Test: pending_req_fr_prompt_agents_md_section
  假设 工作目录无 AGENTS.md
  当 组装提示词时
  那么 不存在空的 agent-instructions section

Scenario: working-directory context 跟随 cwd
  Test: pending_req_fr_prompt_working_directory_context_cwd
  假设 会话工作目录变更
  当 组装 runtime-context 时
  那么 WORKING_DIRECTORY context 反映新 cwd

Scenario: 同序工具名排序与未列工具
  Test: pending_req_fr_prompt_requirement
  假设 两个 section 同 order、工具集含 toolOrder 未列出项
  当 组装时
  那么 同 order 按 name 码元序排列，未列工具按字典序插入 unlisted 标记位

Scenario: section HARNESS_IDENTITY 序值
  Test: pending_req_fr_prompt_section_harness_identity
  假设 读取 SECTION_ORDERS 常量表
  当 查 HARNESS_IDENTITY 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section DEPLOYMENT_PERSONA_PREFIX 序值
  Test: pending_req_fr_prompt_section_deployment_persona_prefix
  假设 读取 SECTION_ORDERS 常量表
  当 查 DEPLOYMENT_PERSONA_PREFIX 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section PLAN_POLICY 序值
  Test: pending_req_fr_prompt_section_plan_policy
  假设 读取 SECTION_ORDERS 常量表
  当 查 PLAN_POLICY 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TEAM_POLICY 序值
  Test: pending_req_fr_prompt_section_team_policy
  假设 读取 SECTION_ORDERS 常量表
  当 查 TEAM_POLICY 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section PTC_ONLY 序值
  Test: pending_req_fr_prompt_section_ptc_only
  假设 读取 SECTION_ORDERS 常量表
  当 查 PTC_ONLY 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section FILE_REFERENCE 序值
  Test: pending_req_fr_prompt_section_file_reference
  假设 读取 SECTION_ORDERS 常量表
  当 查 FILE_REFERENCE 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_BASH 序值
  Test: pending_req_fr_prompt_section_tool_bash
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_BASH 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_PWSH 序值
  Test: pending_req_fr_prompt_section_tool_pwsh
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_PWSH 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_READ 序值
  Test: pending_req_fr_prompt_section_tool_read
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_READ 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_WRITE 序值
  Test: pending_req_fr_prompt_section_tool_write
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WRITE 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_EDIT 序值
  Test: pending_req_fr_prompt_section_tool_edit
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_EDIT 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_GLOB 序值
  Test: pending_req_fr_prompt_section_tool_glob
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_GLOB 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_GREP 序值
  Test: pending_req_fr_prompt_section_tool_grep
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_GREP 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_JOBS 序值
  Test: pending_req_fr_prompt_section_tool_jobs
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_JOBS 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_PTY 序值
  Test: pending_req_fr_prompt_section_tool_pty
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_PTY 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_WEB_SEARCH 序值
  Test: pending_req_fr_prompt_section_tool_web_search
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WEB_SEARCH 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_WEB_FETCH 序值
  Test: pending_req_fr_prompt_section_tool_web_fetch
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WEB_FETCH 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_LSP 序值
  Test: pending_req_fr_prompt_section_tool_lsp
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_LSP 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_SESSION_QUERY 序值
  Test: pending_req_fr_prompt_section_tool_session_query
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_SESSION_QUERY 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_GOAL 序值
  Test: pending_req_fr_prompt_section_tool_goal
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_GOAL 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_WORKFLOW 序值
  Test: pending_req_fr_prompt_section_tool_workflow
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WORKFLOW 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_RALPH 序值
  Test: pending_req_fr_prompt_section_tool_ralph
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_RALPH 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_SUBAGENT 序值
  Test: pending_req_fr_prompt_section_tool_subagent
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_SUBAGENT 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_REPORT 序值
  Test: pending_req_fr_prompt_section_tool_report
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_REPORT 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOL_COMPUTER_USE 序值
  Test: pending_req_fr_prompt_section_tool_computer_use
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_COMPUTER_USE 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section MCP_SERVERS 序值
  Test: pending_req_fr_prompt_section_mcp_servers
  假设 读取 SECTION_ORDERS 常量表
  当 查 MCP_SERVERS 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section TOOLS_SDK 序值
  Test: pending_req_fr_prompt_section_tools_sdk
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOLS_SDK 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section DELIVERABLE_FILE_REFERENCES 序值
  Test: pending_req_fr_prompt_section_deliverable_file_references
  假设 读取 SECTION_ORDERS 常量表
  当 查 DELIVERABLE_FILE_REFERENCES 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section STRUCTURED_OUTPUT 序值
  Test: pending_req_fr_prompt_section_structured_output
  假设 读取 SECTION_ORDERS 常量表
  当 查 STRUCTURED_OUTPUT 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section HARNESS_SOURCE 序值
  Test: pending_req_fr_prompt_section_harness_source
  假设 读取 SECTION_ORDERS 常量表
  当 查 HARNESS_SOURCE 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section WEB_SURFACE 序值
  Test: pending_req_fr_prompt_section_web_surface
  假设 读取 SECTION_ORDERS 常量表
  当 查 WEB_SURFACE 时
  那么 序值与 dsh index.ts:131 全表一致

Scenario: section DEPLOYMENT_PERSONA_SUFFIX 序值
  Test: pending_req_fr_prompt_section_deployment_persona_suffix
  假设 读取 SECTION_ORDERS 常量表
  当 查 DEPLOYMENT_PERSONA_SUFFIX 时
  那么 序值与 REQ-FR-PROMPT-7 全表逐字一致

Scenario: 注入为 user 消息 system-reminder 帧
  Test: pending_req_fr_prompt_user_system_reminder
  假设 工作目录存在 AGENTS.md
  当 组装提示词时
  那么 内容渲染为 user 消息、包裹 system-reminder 帧、首行 "Instructions from: <path>"，而非 system section

Scenario: 变更与删除再注入
  Test: pending_req_fr_prompt_requirement
  假设 会话中 AGENTS.md 被修改或删除
  当 fs 工具触碰该嵌套指令文件后组装时
  那么 以 Updated/removed 语义再注入且触发再投影

Scenario: 尺寸门控与去重
  Test: pending_req_fr_prompt_requirement
  假设 指令文件超 maxSourceBytes 1MiB 或同组重复
  当 加载候选 AGENTS.md/CLAUDE.md 与 local 变体时
  那么 超限整文件跳过、maxBytes 二分截断恰好适配、同组去重、projectRoot 自 cwd 向上找 .git 标记

Scenario: complete section 唯一
  Test: pending_req_fr_prompt_complete_section
  假设 多个提供者竞逐 complete section
  当 waterfall 结算时
  那么 至多一个生效且恢复唯一 prompt section；refreshContext 仅重跑 context 节

Scenario: interpolate false 保留字面
  Test: pending_req_fr_prompt_interpolate_false
  假设 section 声明 interpolate:false 且文本含 {{var}}
  当 组装时
  那么 {{var}} 按字面保留不替换

## Questions

- Source trace: doc/prd.md#10: 提示词组装全部要点；SECTION_ORDERS 与 CONTEXT_ORDERS 全集以 dsh 源文件 :131/:149 为权威, doc/prd.md#22: 提示词全部以脚本从 dsh 源文件抽取生成常量，禁止手抄, confidence: high（PRD 第 10 节）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
