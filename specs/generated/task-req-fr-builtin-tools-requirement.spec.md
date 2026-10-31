spec: task
name: "内置工具集"
tags: [requirements, generated-draft, builtin-tools, tools]
satisfies: [REQ-FR-BUILTIN-TOOLS]
---

## Intent

内置工具是模型能力面。全部工具的参数级 schema 以 dsh docs/tool-catalog.md 为权威，需以生成式文档防漂移；plugin_market 明确不做。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- 第 17 节工具全集 MUST 实现：shell（bash、pwsh、persistent bash/pwsh opt-in）；文件（read、write、edit、read_image、glob、grep、str_replace_editor opt-in）；web（web_search、web_fetch）；任务状态（todo_write、create_goal/get_goal/update_goal、job_kill/job_list/job_output、schedule_create/delete/list/update）；交互（ask_user_question、exit_plan_mode）；会话查询（session_search、session_trace、session_event_read、session_event_search、session_event_trace）；PTY 六件套（terminal_open/read/send/signal/list/close，opt-in）；技能（skill、load_workspace_dependencies）；代码智能（lsp）；目录（working_directory）；内省管理（cordis_inspect_list、cordis_inspect_query、plugin_manager）。
- 每个工具的参数级 schema MUST 对照 dsh docs/tool-catalog.md 冻结，并以生成脚本产出 fly-rafter 的 tool-catalog 供 diff 验证。
- grep 工具 MUST 经内置 ripgrep 子进程（解析 PATH 上的 rg，`FR_RIPGREP` 可指认，打包期捆绑）；缺失时 grep 工具明确报错。
- ask 类工具（ask_user_question 等）在 headless 下 MUST fail-closed。
- MUST NOT 实现 plugin_market。
- web_search 与 web_fetch MUST 支持 provider 可换（deepseek-official / exa / perplexity / http 适配位）。
- shell 组单合同双 crate（Q92-A）：fr-shell 插件承载执行体与作业域（provide shell/subprocess/jobs），fr-tools-builtin provide 全部域服务与工具（Q93-A）；caller-abort kill 走 fr-shell jobs killByToolCall 载体（Q16a-6：fr-tools watcher require jobs 1008，fr-shell D31 grant jobs→tools；body 以切片 wait 释放实例锁承接）。
- 每工具参数级 schema 以 scripts/generate-tool-catalog.py 从 dsh docs/tool-catalog.md 抽取生成常量（Q95-A），--check 钉测防漂移；tool:bash/tool:pwsh 两 systemPrompt section 文本同脚本抽取自 dsh 工具源（2026-10-29 section-text 裁决，PRD 22 规则）；escalation 对、非 promote timeoutMs 文本与 pwsh Windows 附件段为注册时变体，留在组合源并由钉测覆盖。

## Boundaries

### Allowed Changes
- crates/fr-tools-builtin/**
- crates/fr-tools/**
- crates/fr-shell/**
- crates/fr-attachment/**
- crates/fr-llm/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 注册表含第 17 节全部工具
  Test: pending_req_fr_builtin_tools_17
  当 列举 ToolRegistry 时
  那么 含 shell、文件、web、任务状态、交互、会话查询、PTY、技能、lsp、working_directory 与内省管理的全部工具名

Scenario: persistent shell 需 opt-in
  Test: pending_req_fr_builtin_tools_persistent_shell_opt_in
  假设 未启用 persistent shell 配置
  当 列举默认装配的工具时
  那么 无 persistent bash 与 persistent pwsh

Scenario: 工具 schema 与 catalog diff 为空
  Test: generated_catalog_matches_the_dsh_sources
  假设 以生成脚本从真实装配产出 tool-catalog
  当 与冻结的参数表比对时
  那么 diff 为空

Scenario: ripgrep 缺失明确报错
  Test: missing_rg_fails_with_the_launch_text
  假设 PATH 与 FR_RIPGREP 均无 rg
  当 调用 grep 工具时
  那么 返回明确错误而非静默降级

Scenario: FR_RIPGREP 指认生效
  Test: fr_ripgrep_pointer_wins_over_the_bare_probe
  假设 FR_RIPGREP 指向一个 rg 可执行文件
  当 调用 grep 工具时
  那么 使用该指认的 rg 执行

Scenario: headless 下 ask 工具 fail-closed
  Test: pending_req_fr_builtin_tools_headless_ask_fail_closed
  假设 headless 形态调用 ask_user_question
  当 工具执行时
  那么 fail-closed 返回，模型收到拒绝结果

Scenario: 注册表不含 plugin_market
  Test: describe_surface_excludes_plugin_market
  当 列举全部工具与插件名时
  那么 不存在 plugin_market

Scenario: web_search provider 可切换
  Test: pending_req_fr_builtin_tools_web_search_provider
  假设 web 搜索 provider 配置为 exa
  当 调用 web_search 时
  那么 请求经 exa 适配位发出

Scenario: 替代编辑器默认不装配
  Test: baked_bundle_leaves_the_editor_unregistered
  假设 未启用 str_replace_editor 配置
  当 列举默认装配工具时
  那么 无 str_replace_editor

Scenario: run_in_background 产生可管理作业
  Test: background_returns_the_handle_and_escalation_approves
  假设 bash 以 run_in_background 执行
  当 作业运行时
  那么 job_list 可见、job_output 可读、job_kill 可终止

Scenario: todo 状态流转校验
  Test: execute_appends_and_counts_and_refuses_without_an_agent
  假设 todo 列表含 pending、in_progress 与 completed 项
  当 todo_write 提交状态变更时
  那么 非法流转被拒，合法流转写入

Scenario: bash 调度与呈现
  Test: presentations_pin_the_card_shapes
  假设 合约要求的行为已实现
  当 读取工具 bash 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

Scenario: bash 参数越界拒绝
  Test: presentations_pin_the_card_shapes
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: bash 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 bash 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: bash 参数 description 形状
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（主动语态 5-10 词，置于 command 前）] 的名称与类型按目录行实现

Scenario: bash 参数 description 约束
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（主动语态 5-10 词，置于 command 前）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: bash 参数 command 形状
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req（bash -c 命令）] 的名称与类型按目录行实现

Scenario: bash 参数 command 约束
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req（bash -c 命令）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: bash 参数 timeoutMs 形状
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（执行器施加默认与上限；到期转后台 job 而非杀死）] 的名称与类型按目录行实现

Scenario: bash 参数 timeoutMs 约束
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（执行器施加默认与上限；到期转后台 job 而非杀死）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: bash 参数 workdir 形状
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [workdir:string:opt（默认 session workspace）] 的名称与类型按目录行实现

Scenario: bash 参数 workdir 约束
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [workdir:string:opt（默认 session workspace）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: bash 参数 run_in_background 形状
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:boolean:opt（无 job registry 或 enableRunInBackground:false 时不注册）] 的名称与类型按目录行实现

Scenario: bash 参数 run_in_background 约束
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:boolean:opt（无 job registry 或 enableRunInBackground:false 时不注册）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: bash 行为 1
  Test: bash_registration_matches_the_frozen_catalog
  假设 工具 bash 执行时
  当 达到触发条件时
  那么 每次调用全新 shell，勿用 cd 传 workdir；DSH_* 暴露 harness 事实；长输出截尾、完整输出落盘报路径；删除/移动前校验解析后绝对路径；未设变量展开为空串用 ${VAR:?} 防护；沙箱拒绝报 [sandbox: file access denied under <mode

Scenario: pwsh 调度与呈现
  Test: validation_refusals_pin_the_texts
  假设 合约要求的行为已实现
  当 读取工具 pwsh 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

Scenario: pwsh 参数越界拒绝
  Test: validation_refusals_pin_the_texts
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: pwsh 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 pwsh 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: pwsh 行为 1
  Test: pwsh_registration_matches_the_frozen_catalog
  假设 工具 pwsh 执行时
  当 达到触发条件时
  那么 pwsh -Command 方言，每次新进程无持久 PTY；C:\ 原生路径与 $env:NAME；Windows 强杀 [exit code: 1] 视为中断；勿赋值 $HOME 等自动变量；mirror bash 但无沙箱控制

Scenario: persistent-bash 调度与呈现
  Test: pending_req_fr_builtin_tools_persistent_bash
  假设 合约要求的行为已实现
  当 读取工具 persistent bash 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

Scenario: persistent-bash 参数越界拒绝
  Test: pending_req_fr_builtin_tools_persistent_bash
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: persistent-bash 说明 section 注册
  Test: pending_req_fr_builtin_tools_persistent_bash_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 persistent bash 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: persistent-bash 参数 command 形状
  Test: pending_req_fr_builtin_tools_persistent_bash_command
  假设 工具 persistent bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req] 的名称与类型按目录行实现

Scenario: persistent-bash 参数 command 约束
  Test: pending_req_fr_builtin_tools_persistent_bash_command
  假设 工具 persistent bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: persistent-bash 行为 1
  Test: pending_req_fr_builtin_tools_persistent_bash_1
  假设 工具 persistent bash 执行时
  当 达到触发条件时
  那么 owner 隔离持久 PTY，cwd/env 跨调用保持；config backendType 默认 shell、timeoutMs 300000、maxOutputChars 16000

Scenario: persistent-pwsh 调度与呈现
  Test: pending_req_fr_builtin_tools_persistent_pwsh
  假设 合约要求的行为已实现
  当 读取工具 persistent pwsh 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

Scenario: persistent-pwsh 参数越界拒绝
  Test: pending_req_fr_builtin_tools_persistent_pwsh
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: persistent-pwsh 说明 section 注册
  Test: pending_req_fr_builtin_tools_persistent_pwsh_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 persistent pwsh 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: read 调度与呈现
  Test: read_round_trip_pins_the_shapes
  假设 合约要求的行为已实现
  当 读取工具 read 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: read 参数越界拒绝
  Test: read_refusals_pin_the_texts
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: read 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 read 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: read 参数 file_path 形状
  Test: read_registration_matches_the_frozen_catalog
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的名称与类型按目录行实现

Scenario: read 参数 file_path 约束
  Test: read_registration_matches_the_frozen_catalog
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: read 参数 offset 形状
  Test: read_registration_matches_the_frozen_catalog
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [offset:number:opt:1（1-based 起始行）] 的名称与类型按目录行实现

Scenario: read 参数 offset 约束
  Test: read_registration_matches_the_frozen_catalog
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [offset:number:opt:1（1-based 起始行）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: read 参数 limit 形状
  Test: read_registration_matches_the_frozen_catalog
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [limit:number:opt:2000（最大行数）] 的名称与类型按目录行实现

Scenario: read 参数 limit 约束
  Test: read_registration_matches_the_frozen_catalog
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [limit:number:opt:2000（最大行数）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: read 行为 1
  Test: read_round_trip_pins_the_shapes
  假设 工具 read 执行时
  当 达到触发条件时
  那么 返回行号内容；config readLimit（默认兼上限）/readMaxLineLength/readMaxBytes/readStreamMinSize（超限转流）；presence/absence 发 fs/observed

Scenario: read_image 调度与呈现
  Test: presentation_faces_pin_the_read_family_card_and_path_meta
  假设 合约要求的行为已实现
  当 读取工具 read_image 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: read_image 参数越界拒绝
  Test: blank_and_unsupported_paths_refuse_before_any_io
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: read_image 说明 section 注册缺席
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 read_image 无说明 section（dsh 真源 read-image.ts 不注入 systemPrompt）；注册表恰七条 tool:* section 且 order 位于 1000-3000

Scenario: read_image 参数 file_path 形状
  Test: read_image_registration_matches_the_frozen_catalog
  假设 工具 read_image 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的名称与类型按目录行实现

Scenario: read_image 参数 file_path 约束
  Test: read_image_registration_matches_the_frozen_catalog
  假设 工具 read_image 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: read_image 行为 1
  Test: happy_path_saves_observes_and_returns_the_reference
  假设 工具 read_image 执行时
  当 达到触发条件时
  那么 PNG/JPEG/WebP/GIF 返回图像本体，大图自动缩；无 ctx.attachments 不注册；仅精确路由模型声明 image input 才执行否则拒绝

Scenario: write 调度与呈现
  Test: edit_family_pins_the_arms
  假设 合约要求的行为已实现
  当 读取工具 write 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 diff

Scenario: write 参数越界拒绝
  Test: write_create_then_guarded_update_round_trips
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: write 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 write 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: write 参数 file_path 形状
  Test: write_registration_matches_the_frozen_catalog
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 content）] 的名称与类型按目录行实现

Scenario: write 参数 file_path 约束
  Test: write_registration_matches_the_frozen_catalog
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 content）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: write 参数 content 形状
  Test: write_registration_matches_the_frozen_catalog
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [content:string:req（完整 UTF-8）] 的名称与类型按目录行实现

Scenario: write 参数 content 约束
  Test: write_registration_matches_the_frozen_catalog
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [content:string:req（完整 UTF-8）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: write 行为 1
  Test: write_create_then_guarded_update_round_trips
  假设 工具 write 执行时
  当 达到触发条件时
  那么 创建或整文件替换；发 fs/write-intent；read-before-write 由 fs-observation-policy 施加

Scenario: edit 调度与呈现
  Test: edit_family_pins_the_arms
  假设 合约要求的行为已实现
  当 读取工具 edit 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 diff

Scenario: edit 参数越界拒绝
  Test: edit_family_pins_the_arms
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: edit 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 edit 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: edit 参数 file_path 形状
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 old/new）] 的名称与类型按目录行实现

Scenario: edit 参数 file_path 约束
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 old/new）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: edit 参数 old_string 形状
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [old_string:string:req] 的名称与类型按目录行实现

Scenario: edit 参数 old_string 约束
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [old_string:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: edit 参数 new_string 形状
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [new_string:string:req（空串即删除）] 的名称与类型按目录行实现

Scenario: edit 参数 new_string 约束
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [new_string:string:req（空串即删除）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: edit 参数 replace_all 形状
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [replace_all:boolean:opt:false（false 时 old_string 须恰好出现一次）] 的名称与类型按目录行实现

Scenario: edit 参数 replace_all 约束
  Test: edit_registration_matches_the_frozen_catalog
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [replace_all:boolean:opt:false（false 时 old_string 须恰好出现一次）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: edit 行为 1
  Test: edit_family_pins_the_arms
  假设 工具 edit 执行时
  当 达到触发条件时
  那么 UTF-8 字面替换；发 fs/edit-intent；read-before-write 同 write

Scenario: glob 调度与呈现
  Test: glob_round_trip_pins_the_shapes
  假设 合约要求的行为已实现
  当 读取工具 glob 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

Scenario: glob 参数越界拒绝
  Test: argument_refusals_pin_the_texts
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: glob 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 glob 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: glob 参数 pattern 形状
  Test: glob_registration_matches_the_frozen_catalog
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（无分隔符按任意深度 basename 匹配，带分隔符锚定深度）] 的名称与类型按目录行实现

Scenario: glob 参数 pattern 约束
  Test: glob_registration_matches_the_frozen_catalog
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（无分隔符按任意深度 basename 匹配，带分隔符锚定深度）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: glob 参数 path 形状
  Test: glob_registration_matches_the_frozen_catalog
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt（默认 session workspace）] 的名称与类型按目录行实现

Scenario: glob 参数 path 约束
  Test: glob_registration_matches_the_frozen_catalog
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt（默认 session workspace）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: glob 行为 1
  Test: glob_round_trip_pins_the_shapes
  假设 工具 glob 执行时
  当 达到触发条件时
  那么 打包 ripgrep 经 ctx.subprocess 前台运行（永不后台 job、无 host rg 依赖、无 shell 层）；返回 ≤100 路径按 mtime 序；超限按 sampleOverCapGlobResults 顶层采样并报完整列表保存位置（spill 落盘）

Scenario: grep 调度与呈现
  Test: grep_round_trip_pins_the_shapes
  假设 合约要求的行为已实现
  当 读取工具 grep 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

Scenario: grep 参数越界拒绝
  Test: argument_refusals_pin_the_texts
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: grep 说明 section 注册
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 grep 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: grep 参数 pattern 形状
  Test: grep_registration_matches_the_frozen_catalog
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（ripgrep 正则）] 的名称与类型按目录行实现

Scenario: grep 参数 pattern 约束
  Test: grep_registration_matches_the_frozen_catalog
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（ripgrep 正则）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: grep 参数 path 形状
  Test: grep_registration_matches_the_frozen_catalog
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt] 的名称与类型按目录行实现

Scenario: grep 参数 path 约束
  Test: grep_registration_matches_the_frozen_catalog
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: grep 参数 include 形状
  Test: grep_registration_matches_the_frozen_catalog
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [include:string:opt（单 glob，不支持否定）] 的名称与类型按目录行实现

Scenario: grep 参数 include 约束
  Test: grep_registration_matches_the_frozen_catalog
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [include:string:opt（单 glob，不支持否定）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: grep 行为 1
  Test: grep_round_trip_pins_the_shapes
  假设 工具 grep 执行时
  当 达到触发条件时
  那么 匹配行+行号按文件分组 ≤250 匹配；超限报完整列表位置；config globMaxResults/grepMaxMatches/grepMaxLineBytes/searchMetaMaxBytes/rawOutputMaxBytes（溢出 SEARCH_RAW_OUTPUT_OVERFLOW

Scenario: str_replace_editor 调度与呈现
  Test: present_call_pins_the_four_arms
  假设 合约要求的行为已实现
  当 读取工具 str_replace_editor 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 diff(str_replace/create)+generic(view)

Scenario: str_replace_editor 参数越界拒绝
  Test: required_parameter_refusals_pin_the_texts
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: str_replace_editor 说明逐字与呈现四臂
  Test: present_call_pins_the_four_arms
  假设 工具 str_replace_editor 已注册且组装系统提示词时
  当 扫描 section 注册表与读取工具声明时
  那么 工具 str_replace_editor 无说明 section（dsh 真源不注入 systemPrompt）；description 12 句逐字抽取；presentCall 四臂=view 读卡/create 与 str_replace diff 卡/insert 行锚编辑卡

Scenario: str_replace_editor 参数 command 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req:enum[view|create|str_replace|insert]] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 command 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req:enum[view|create|str_replace|insert]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 参数 path 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [path:string:req（绝对路径）] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 path 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [path:string:req（绝对路径）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 参数 file_text 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [file_text:string|null:opt（create 必填，已存在不可 create）] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 file_text 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [file_text:string|null:opt（create 必填，已存在不可 create）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 参数 insert_line 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [insert_line:integer|null:opt（insert 必填）] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 insert_line 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [insert_line:integer|null:opt（insert 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 参数 new_str 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [new_str:string|null:opt（str_replace 可选=删除，insert 必填）] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 new_str 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [new_str:string|null:opt（str_replace 可选=删除，insert 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 参数 old_str 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [old_str:string|null:opt（str_replace 必填，须唯一匹配否则不替换）] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 old_str 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [old_str:string|null:opt（str_replace 必填，须唯一匹配否则不替换）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 参数 view_range 形状
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [view_range:array<integer>|null:opt（1-based，[start,-1] 到末尾）] 的名称与类型按目录行实现

Scenario: str_replace_editor 参数 view_range 约束
  Test: str_replace_editor_registration_matches_the_frozen_catalog
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [view_range:array<integer>|null:opt（1-based，[start,-1] 到末尾）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: str_replace_editor 行为 1
  Test: view_family_pins_the_arms
  假设 工具 str_replace_editor 执行时
  当 达到触发条件时
  那么 view=cat -n；目录 view 列非隐藏两层；长输出截断标 <response clipped>；未用命令的 null 占位视为省略

Scenario: web_search 调度与呈现
  Test: pending_req_fr_builtin_tools_web_search
  假设 合约要求的行为已实现
  当 读取工具 web_search 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search+web

Scenario: web_search 参数越界拒绝
  Test: pending_req_fr_builtin_tools_web_search
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: web_search 说明 section 注册
  Test: pending_req_fr_builtin_tools_web_search_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 web_search 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: web_search 参数 queries 形状
  Test: pending_req_fr_builtin_tools_web_search_queries
  假设 工具 web_search 已注册
  当 校验 schema 时
  那么 参数行 [queries:array<string>:req（1-4 条，结果合并）] 的名称与类型按目录行实现

Scenario: web_search 参数 queries 约束
  Test: pending_req_fr_builtin_tools_web_search_queries
  假设 工具 web_search 已注册
  当 校验 schema 时
  那么 参数行 [queries:array<string>:req（1-4 条，结果合并）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: web_search 行为 1
  Test: pending_req_fr_builtin_tools_web_search_1
  假设 工具 web_search 执行时
  当 达到触发条件时
  那么 可选摘要答案+源 URL 列表；config searchTimeoutMs 30000、searchMaxQueries/searchMaxResults 上限；后端 deepseek/exa/perplexity

Scenario: web_fetch 调度与呈现
  Test: pending_req_fr_builtin_tools_web_fetch
  假设 合约要求的行为已实现
  当 读取工具 web_fetch 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 fetch+web

Scenario: web_fetch 参数越界拒绝
  Test: pending_req_fr_builtin_tools_web_fetch
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: web_fetch 说明 section 注册
  Test: pending_req_fr_builtin_tools_web_fetch_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 web_fetch 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: web_fetch 参数 url 形状
  Test: pending_req_fr_builtin_tools_web_fetch_url
  假设 工具 web_fetch 已注册
  当 校验 schema 时
  那么 参数行 [url:string:req（HTTP(S)）] 的名称与类型按目录行实现

Scenario: web_fetch 参数 url 约束
  Test: pending_req_fr_builtin_tools_web_fetch_url
  假设 工具 web_fetch 已注册
  当 校验 schema 时
  那么 参数行 [url:string:req（HTTP(S)）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: web_fetch 行为 1
  Test: pending_req_fr_builtin_tools_web_fetch_1
  假设 工具 web_fetch 执行时
  当 达到触发条件时
  那么 解码为文本；config fetchTimeoutMs 30000、fetchMaxOutputChars 200000、search/fetch 注册开关默认 true

Scenario: todo_write 调度与呈现
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 todo_write 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 other

Scenario: todo_write 参数越界拒绝
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: todo_write 说明 section 注册缺席
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 todo_write 无说明 section（dsh 真源 tool-todo 不注入 systemPrompt）；注册表恰七条 tool:* section 且 order 位于 1000-3000

Scenario: todo_write 参数 todos 形状
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos:array<object>:req（完整表替换）] 的名称与类型按目录行实现

Scenario: todo_write 参数 todos 约束
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos:array<object>:req（完整表替换）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: todo_write 参数 todos[].content 形状
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].content:string:req] 的名称与类型按目录行实现

Scenario: todo_write 参数 todos[].content 约束
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].content:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: todo_write 参数 todos[].status 形状
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].status:string:req:enum[pending|in_progress|completed]] 的名称与类型按目录行实现

Scenario: todo_write 参数 todos[].status 约束
  Test: todo_write_registration_matches_the_frozen_catalog
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].status:string:req:enum[pending|in_progress|completed]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: todo_write 行为 1
  Test: execute_appends_and_counts_and_refuses_without_an_agent
  假设 工具 todo_write 执行时
  当 达到触发条件时
  那么 session 持有状态；并行工作可多个 in_progress（allowParallelInProgress:true），false 则恰一活跃且多标拒绝

Scenario: create_goal 调度与呈现
  Test: goal_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 create_goal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 other

Scenario: create_goal 参数越界拒绝
  Test: goal_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: create_goal 说明 section 注册
  Test: goal_section_composes_over_the_extracted_template
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 create_goal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: create_goal 参数 objective 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:req（直接人类请求推断的长时目标）] 的名称与类型按目录行实现

Scenario: create_goal 参数 objective 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:req（直接人类请求推断的长时目标）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: create_goal 参数 max_goal_rounds 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（正安全整数）] 的名称与类型按目录行实现

Scenario: create_goal 参数 max_goal_rounds 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（正安全整数）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: create_goal 行为 1
  Test: create_flow_commits_over_the_human_turn
  假设 工具 create_goal 执行时
  当 达到触发条件时
  那么 跨自动续轮；仅长时目标

Scenario: get_goal 调度与呈现
  Test: goal_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 get_goal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: get_goal 参数越界拒绝
  Test: goal_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: get_goal 说明 section 注册
  Test: goal_section_composes_over_the_extracted_template
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 get_goal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: get_goal 行为 1
  Test: get_flow_reads_the_current_goal
  假设 工具 get_goal 执行时
  当 达到触发条件时
  那么 读当前 goal 含 id/revision

Scenario: update_goal 调度与呈现
  Test: goal_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 update_goal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: update_goal 参数越界拒绝
  Test: goal_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: update_goal 说明 section 注册
  Test: goal_section_composes_over_the_extracted_template
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 update_goal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: update_goal 参数 goal_id 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [goal_id:string:req] 的名称与类型按目录行实现

Scenario: update_goal 参数 goal_id 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [goal_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: update_goal 参数 revision 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [revision:number:req（精确正 revision）] 的名称与类型按目录行实现

Scenario: update_goal 参数 revision 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [revision:number:req（精确正 revision）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: update_goal 参数 action 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[edit|pause|resume|complete|blocked]] 的名称与类型按目录行实现

Scenario: update_goal 参数 action 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[edit|pause|resume|complete|blocked]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: update_goal 参数 objective 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:opt（仅 edit）] 的名称与类型按目录行实现

Scenario: update_goal 参数 objective 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:opt（仅 edit）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: update_goal 参数 max_goal_rounds 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（仅 edit）] 的名称与类型按目录行实现

Scenario: update_goal 参数 max_goal_rounds 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（仅 edit）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: update_goal 参数 blocked_reason 形状
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [blocked_reason:string:opt（仅 blocked 必填）] 的名称与类型按目录行实现

Scenario: update_goal 参数 blocked_reason 约束
  Test: goal_registrations_match_the_frozen_catalog
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [blocked_reason:string:opt（仅 blocked 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: update_goal 行为 1
  Test: update_flow_pins_the_action_families
  假设 工具 update_goal 执行时
  当 达到触发条件时
  那么 edit/pause/resume 需直接人类 root 权威；blocked 在最小轮前被拒（blockedAfterConsecutiveRounds 默认 3）

Scenario: job_kill 调度与呈现
  Test: job_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 job_kill 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 execute

Scenario: job_kill 参数越界拒绝
  Test: job_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: job_kill 说明 section 注册
  Test: jobs_section_pins_the_extracted_text
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 job_kill 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: job_kill 参数 job_id 形状
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的名称与类型按目录行实现

Scenario: job_kill 参数 job_id 约束
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: job_kill 参数 reason 形状
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [reason:string:opt（入日志并转发）] 的名称与类型按目录行实现

Scenario: job_kill 参数 reason 约束
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [reason:string:opt（入日志并转发）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: job_list 调度与呈现
  Test: job_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 job_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: job_list 参数越界拒绝
  Test: job_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: job_list 说明 section 注册
  Test: jobs_section_pins_the_extracted_text
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 job_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: job_list 行为 1
  Test: output_and_list_bodies_drive_the_registry_faces
  假设 工具 job_list 执行时
  当 达到触发条件时
  那么 列 running+finished 的 id/kind/status

Scenario: job_output 调度与呈现
  Test: job_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 job_output 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: job_output 参数越界拒绝
  Test: job_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: job_output 说明 section 注册
  Test: jobs_section_pins_the_extracted_text
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 job_output 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: job_output 参数 job_id 形状
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的名称与类型按目录行实现

Scenario: job_output 参数 job_id 约束
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: job_output 参数 wait 形状
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [wait:boolean:opt:false] 的名称与类型按目录行实现

Scenario: job_output 参数 wait 约束
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [wait:boolean:opt:false] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: job_output 参数 timeout_ms 形状
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [timeout_ms:number:opt（默认 30s，钳至 maxWaitTimeoutMs 10min）] 的名称与类型按目录行实现

Scenario: job_output 参数 timeout_ms 约束
  Test: job_registrations_match_the_frozen_catalog
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [timeout_ms:number:opt（默认 30s，钳至 maxWaitTimeoutMs 10min）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: job_output 行为 1
  Test: output_and_list_bodies_drive_the_registry_faces
  假设 工具 job_output 执行时
  当 达到触发条件时
  那么 流式增量读取；wait 超时留 job 运行

Scenario: schedule_create 调度与呈现
  Test: schedule_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 schedule_create 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 other

Scenario: schedule_create 参数越界拒绝
  Test: schedule_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: schedule_create 说明 section 缺席
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 无工具 schedule_create 的说明 section（dsh 真源 tool-schedule 不注入 systemPrompt）；注册表恰九条 tool:* section 且 order 位于 1000-3000

Scenario: schedule_create 参数 prompt 形状
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req] 的名称与类型按目录行实现

Scenario: schedule_create 参数 prompt 约束
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: schedule_create 参数 title 形状
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [title:string:req（≤120 字符规则）] 的名称与类型按目录行实现

Scenario: schedule_create 参数 title 约束
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [title:string:req（≤120 字符规则）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: schedule_create 行为 1
  Test: the_management_flows_drive_the_storage_rows
  假设 工具 schedule_create 执行时
  当 达到触发条件时
  那么 时区 UTC 或 IANA；不存在本地时刻跳过、重复取较早；宕机后循环提醒只补最近一次；管理走 storage domain

Scenario: schedule_delete 调度与呈现
  Test: schedule_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 schedule_delete 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: schedule_delete 参数越界拒绝
  Test: schedule_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: schedule_delete 说明 section 缺席
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 无工具 schedule_delete 的说明 section（dsh 真源 tool-schedule 不注入 systemPrompt）；注册表恰九条 tool:* section 且 order 位于 1000-3000

Scenario: schedule_delete 参数 id 形状
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_delete 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的名称与类型按目录行实现

Scenario: schedule_delete 参数 id 约束
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_delete 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: schedule_delete 行为 1
  Test: the_management_flows_drive_the_storage_rows
  假设 工具 schedule_delete 执行时
  当 达到触发条件时
  那么 活动/非活动均可删，不撤回已入队

Scenario: schedule_list 调度与呈现
  Test: schedule_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 schedule_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: schedule_list 参数越界拒绝
  Test: schedule_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: schedule_list 说明 section 缺席
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 无工具 schedule_list 的说明 section（dsh 真源 tool-schedule 不注入 systemPrompt）；注册表恰九条 tool:* section 且 order 位于 1000-3000

Scenario: schedule_list 行为 1
  Test: the_management_flows_drive_the_storage_rows
  假设 工具 schedule_list 执行时
  当 达到触发条件时
  那么 列当前 session 活动提醒

Scenario: schedule_update 调度与呈现
  Test: schedule_registrations_match_the_frozen_catalog
  假设 合约要求的行为已实现
  当 读取工具 schedule_update 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: schedule_update 参数越界拒绝
  Test: schedule_registrations_match_the_frozen_catalog
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: schedule_update 说明 section 缺席
  Test: prompt_sections_pin_the_nine_tools
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 无工具 schedule_update 的说明 section（dsh 真源 tool-schedule 不注入 systemPrompt）；注册表恰九条 tool:* section 且 order 位于 1000-3000

Scenario: schedule_update 参数 id 形状
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_update 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的名称与类型按目录行实现

Scenario: schedule_update 参数 id 约束
  Test: schedule_registrations_match_the_frozen_catalog
  假设 工具 schedule_update 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: schedule_update 行为 1
  Test: the_management_flows_drive_the_storage_rows
  假设 工具 schedule_update 执行时
  当 达到触发条件时
  那么 原地改保留 id；改相对延迟须新建；every_seconds 改动按保存时刻重对齐

Scenario: ask_user_question 调度与呈现
  Test: pending_req_fr_builtin_tools_ask_user_question
  假设 合约要求的行为已实现
  当 读取工具 ask_user_question 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: ask_user_question 参数越界拒绝
  Test: pending_req_fr_builtin_tools_ask_user_question
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: ask_user_question 说明 section 注册
  Test: pending_req_fr_builtin_tools_ask_user_question_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 ask_user_question 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: ask_user_question 参数 questions 形状
  Test: pending_req_fr_builtin_tools_ask_user_question_questions
  假设 工具 ask_user_question 已注册
  当 校验 schema 时
  那么 参数行 [questions:array<object>:req（[].id:req 稳定 id、[].question:req、[].header:opt、[].options:opt（label 必填+description；推荐首位加 (Recommended)）、[].multi_select:opt:false）] 的名称与类型按目录行实现

Scenario: ask_user_question 参数 questions 约束
  Test: pending_req_fr_builtin_tools_ask_user_question_questions
  假设 工具 ask_user_question 已注册
  当 校验 schema 时
  那么 参数行 [questions:array<object>:req（[].id:req 稳定 id、[].question:req、[].header:opt、[].options:opt（label 必填+description；推荐首位加 (Recommended)）、[].multi_select:opt:false）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: ask_user_question 行为 1
  Test: pending_req_fr_builtin_tools_ask_user_question_1
  假设 工具 ask_user_question 执行时
  当 达到触发条件时
  那么 默认阻塞等回答；config mode:timed 启用超时+pending result（timeout 默认 120s，-1 无限阻塞）

Scenario: exit_plan_mode 调度与呈现
  Test: pending_req_fr_builtin_tools_exit_plan_mode
  假设 合约要求的行为已实现
  当 读取工具 exit_plan_mode 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: exit_plan_mode 参数越界拒绝
  Test: pending_req_fr_builtin_tools_exit_plan_mode
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: exit_plan_mode 说明 section 注册
  Test: pending_req_fr_builtin_tools_exit_plan_mode_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 exit_plan_mode 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: exit_plan_mode 参数 plan 形状
  Test: pending_req_fr_builtin_tools_exit_plan_mode_plan
  假设 工具 exit_plan_mode 已注册
  当 校验 schema 时
  那么 参数行 [plan:string:req（完整 markdown，# 标题开头）] 的名称与类型按目录行实现

Scenario: exit_plan_mode 参数 plan 约束
  Test: pending_req_fr_builtin_tools_exit_plan_mode_plan
  假设 工具 exit_plan_mode 已注册
  当 校验 schema 时
  那么 参数行 [plan:string:req（完整 markdown，# 标题开头）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: exit_plan_mode 行为 1
  Test: pending_req_fr_builtin_tools_exit_plan_mode_1
  假设 工具 exit_plan_mode 执行时
  当 达到触发条件时
  那么 schema 常驻防 churn；plan 外 execute 拒绝；plan 内经 user-questions 呈批 approve/继续规划；批准在 step 边界记录 inactive

Scenario: run_code 调度与呈现
  Test: pending_req_fr_builtin_tools_run_code
  假设 合约要求的行为已实现
  当 读取工具 run_code 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic（PTC）

Scenario: run_code 参数越界拒绝
  Test: pending_req_fr_builtin_tools_run_code
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: run_code 说明 section 注册
  Test: pending_req_fr_builtin_tools_run_code_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 run_code 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: run_code 参数 code 形状
  Test: pending_req_fr_builtin_tools_run_code_code
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [code:string:req（TypeScript 程序）] 的名称与类型按目录行实现

Scenario: run_code 参数 code 约束
  Test: pending_req_fr_builtin_tools_run_code_code
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [code:string:req（TypeScript 程序）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: run_code 参数 description 形状
  Test: pending_req_fr_builtin_tools_run_code_description
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（5-10 词）] 的名称与类型按目录行实现

Scenario: run_code 参数 description 约束
  Test: pending_req_fr_builtin_tools_run_code_description
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（5-10 词）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: run_code 参数 timeoutMs 形状
  Test: pending_req_fr_builtin_tools_run_code_timeoutms
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（正数，受部署上限封顶）] 的名称与类型按目录行实现

Scenario: run_code 参数 timeoutMs 约束
  Test: pending_req_fr_builtin_tools_run_code_timeoutms
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（正数，受部署上限封顶）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: run_code 参数 sandbox_permissions 形状
  Test: pending_req_fr_builtin_tools_run_code_sandbox_permissions
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [sandbox_permissions:string:opt:enum[workspace-write|danger-full-access]] 的名称与类型按目录行实现

Scenario: run_code 参数 sandbox_permissions 约束
  Test: pending_req_fr_builtin_tools_run_code_sandbox_permissions
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [sandbox_permissions:string:opt:enum[workspace-write|danger-full-access]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: run_code 参数 justification 形状
  Test: pending_req_fr_builtin_tools_run_code_justification
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [justification:string:opt（用户当前请求语言）] 的名称与类型按目录行实现

Scenario: run_code 参数 justification 约束
  Test: pending_req_fr_builtin_tools_run_code_justification
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [justification:string:opt（用户当前请求语言）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: run_code 行为 1
  Test: pending_req_fr_builtin_tools_run_code_1
  假设 工具 run_code 执行时
  当 达到触发条件时
  那么 mode:ptc 保留传输不可过滤；子调用经完整 guarded pipeline；并发安全子调用重叠至 maxParallelSubCalls（默认 10，1=串行）；图片型子工具结果 run 后附加

Scenario: session_search 调度与呈现
  Test: pending_req_fr_builtin_tools_session_search
  假设 合约要求的行为已实现
  当 读取工具 session_search 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

Scenario: session_search 参数越界拒绝
  Test: pending_req_fr_builtin_tools_session_search
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: session_search 说明 section 注册
  Test: pending_req_fr_builtin_tools_session_search_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_search 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: session_search 参数 query 形状
  Test: pending_req_fr_builtin_tools_session_search_query
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的名称与类型按目录行实现

Scenario: session_search 参数 query 约束
  Test: pending_req_fr_builtin_tools_session_search_query
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_search 参数 availability 形状
  Test: pending_req_fr_builtin_tools_session_search_availability
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [availability:enum[live|persisted]:opt（至少一选）] 的名称与类型按目录行实现

Scenario: session_search 参数 availability 约束
  Test: pending_req_fr_builtin_tools_session_search_availability
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [availability:enum[live|persisted]:opt（至少一选）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_search 行为 1
  Test: pending_req_fr_builtin_tools_session_search_1
  假设 工具 session_search 执行时
  当 达到触发条件时
  那么 跨 workspace 搜先前会话每会话回最强匹配；config maxSearchResults 100、searchTimeoutMs 30000

Scenario: session_trace 调度与呈现
  Test: pending_req_fr_builtin_tools_session_trace
  假设 合约要求的行为已实现
  当 读取工具 session_trace 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: session_trace 参数越界拒绝
  Test: pending_req_fr_builtin_tools_session_trace
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: session_trace 说明 section 注册
  Test: pending_req_fr_builtin_tools_session_trace_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_trace 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: session_trace 参数 session_id 形状
  Test: pending_req_fr_builtin_tools_session_trace_session_id
  假设 工具 session_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt（省略当前）] 的名称与类型按目录行实现

Scenario: session_trace 参数 session_id 约束
  Test: pending_req_fr_builtin_tools_session_trace_session_id
  假设 工具 session_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt（省略当前）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_trace 行为 1
  Test: pending_req_fr_builtin_tools_session_trace_1
  假设 工具 session_trace 执行时
  当 达到触发条件时
  那么 读授权谱系

Scenario: session_event_read 调度与呈现
  Test: pending_req_fr_builtin_tools_session_event_read
  假设 合约要求的行为已实现
  当 读取工具 session_event_read 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: session_event_read 参数越界拒绝
  Test: pending_req_fr_builtin_tools_session_event_read
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: session_event_read 说明 section 注册
  Test: pending_req_fr_builtin_tools_session_event_read_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_event_read 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: session_event_read 参数 seq 形状
  Test: pending_req_fr_builtin_tools_session_event_read_seq
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的名称与类型按目录行实现

Scenario: session_event_read 参数 seq 约束
  Test: pending_req_fr_builtin_tools_session_event_read_seq
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_event_read 参数 session_id 形状
  Test: pending_req_fr_builtin_tools_session_event_read_session_id
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的名称与类型按目录行实现

Scenario: session_event_read 参数 session_id 约束
  Test: pending_req_fr_builtin_tools_session_event_read_session_id
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_event_search 调度与呈现
  Test: pending_req_fr_builtin_tools_session_event_search
  假设 合约要求的行为已实现
  当 读取工具 session_event_search 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

Scenario: session_event_search 参数越界拒绝
  Test: pending_req_fr_builtin_tools_session_event_search
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: session_event_search 说明 section 注册
  Test: pending_req_fr_builtin_tools_session_event_search_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_event_search 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: session_event_search 参数 query 形状
  Test: pending_req_fr_builtin_tools_session_event_search_query
  假设 工具 session_event_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的名称与类型按目录行实现

Scenario: session_event_search 参数 query 约束
  Test: pending_req_fr_builtin_tools_session_event_search_query
  假设 工具 session_event_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_event_search 行为 1
  Test: pending_req_fr_builtin_tools_session_event_search_1
  假设 工具 session_event_search 执行时
  当 达到触发条件时
  那么 排除执行本调用的 step

Scenario: session_event_trace 调度与呈现
  Test: pending_req_fr_builtin_tools_session_event_trace
  假设 合约要求的行为已实现
  当 读取工具 session_event_trace 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: session_event_trace 参数越界拒绝
  Test: pending_req_fr_builtin_tools_session_event_trace
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: session_event_trace 说明 section 注册
  Test: pending_req_fr_builtin_tools_session_event_trace_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_event_trace 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: session_event_trace 参数 seq 形状
  Test: pending_req_fr_builtin_tools_session_event_trace_seq
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的名称与类型按目录行实现

Scenario: session_event_trace 参数 seq 约束
  Test: pending_req_fr_builtin_tools_session_event_trace_seq
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_event_trace 参数 session_id 形状
  Test: pending_req_fr_builtin_tools_session_event_trace_session_id
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的名称与类型按目录行实现

Scenario: session_event_trace 参数 session_id 约束
  Test: pending_req_fr_builtin_tools_session_event_trace_session_id
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: session_event_trace 行为 1
  Test: pending_req_fr_builtin_tools_session_event_trace_1
  假设 工具 session_event_trace 执行时
  当 达到触发条件时
  那么 读直接替换与关系

Scenario: terminal_open 调度与呈现
  Test: pending_req_fr_builtin_tools_terminal_open
  假设 合约要求的行为已实现
  当 读取工具 terminal_open 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 execute

Scenario: terminal_open 参数越界拒绝
  Test: pending_req_fr_builtin_tools_terminal_open
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: terminal_open 说明 section 注册
  Test: pending_req_fr_builtin_tools_terminal_open_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_open 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: terminal_open 参数 type 形状
  Test: pending_req_fr_builtin_tools_terminal_open_type
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [type:string:req（通常 shell）] 的名称与类型按目录行实现

Scenario: terminal_open 参数 type 约束
  Test: pending_req_fr_builtin_tools_terminal_open_type
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [type:string:req（通常 shell）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_open 参数 name 形状
  Test: pending_req_fr_builtin_tools_terminal_open_name
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [name:opt] 的名称与类型按目录行实现

Scenario: terminal_open 参数 name 约束
  Test: pending_req_fr_builtin_tools_terminal_open_name
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [name:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_open 参数 cwd 形状
  Test: pending_req_fr_builtin_tools_terminal_open_cwd
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（非绝对按 Session 目录解析）] 的名称与类型按目录行实现

Scenario: terminal_open 参数 cwd 约束
  Test: pending_req_fr_builtin_tools_terminal_open_cwd
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（非绝对按 Session 目录解析）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_read 调度与呈现
  Test: pending_req_fr_builtin_tools_terminal_read
  假设 合约要求的行为已实现
  当 读取工具 terminal_read 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: terminal_read 参数越界拒绝
  Test: pending_req_fr_builtin_tools_terminal_read
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: terminal_read 说明 section 注册
  Test: pending_req_fr_builtin_tools_terminal_read_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_read 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: terminal_read 参数 sessionId 形状
  Test: pending_req_fr_builtin_tools_terminal_read_sessionid
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

Scenario: terminal_read 参数 sessionId 约束
  Test: pending_req_fr_builtin_tools_terminal_read_sessionid
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_read 参数 offset 形状
  Test: pending_req_fr_builtin_tools_terminal_read_offset
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的名称与类型按目录行实现

Scenario: terminal_read 参数 offset 约束
  Test: pending_req_fr_builtin_tools_terminal_read_offset
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_read 参数 count 形状
  Test: pending_req_fr_builtin_tools_terminal_read_count
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [count:opt:500（后端封顶）] 的名称与类型按目录行实现

Scenario: terminal_read 参数 count 约束
  Test: pending_req_fr_builtin_tools_terminal_read_count
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [count:opt:500（后端封顶）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_send 调度与呈现
  Test: pending_req_fr_builtin_tools_terminal_send
  假设 合约要求的行为已实现
  当 读取工具 terminal_send 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: terminal_send 参数越界拒绝
  Test: pending_req_fr_builtin_tools_terminal_send
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: terminal_send 说明 section 注册
  Test: pending_req_fr_builtin_tools_terminal_send_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_send 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: terminal_send 参数 sessionId 形状
  Test: pending_req_fr_builtin_tools_terminal_send_sessionid
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

Scenario: terminal_send 参数 sessionId 约束
  Test: pending_req_fr_builtin_tools_terminal_send_sessionid
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_send 参数 text 形状
  Test: pending_req_fr_builtin_tools_terminal_send_text
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [text:string:req] 的名称与类型按目录行实现

Scenario: terminal_send 参数 text 约束
  Test: pending_req_fr_builtin_tools_terminal_send_text
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [text:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_send 参数 submit 形状
  Test: pending_req_fr_builtin_tools_terminal_send_submit
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [submit:boolean:opt:true] 的名称与类型按目录行实现

Scenario: terminal_send 参数 submit 约束
  Test: pending_req_fr_builtin_tools_terminal_send_submit
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [submit:boolean:opt:true] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_send 参数 run_in_background 形状
  Test: pending_req_fr_builtin_tools_terminal_send_run_in_background
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:opt（立即返回 job id）] 的名称与类型按目录行实现

Scenario: terminal_send 参数 run_in_background 约束
  Test: pending_req_fr_builtin_tools_terminal_send_run_in_background
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:opt（立即返回 job id）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_send 行为 1
  Test: pending_req_fr_builtin_tools_terminal_send_1
  假设 工具 terminal_send 执行时
  当 达到触发条件时
  那么 默认等 prompt/静默/超时/退出；后台经 ctx.jobs

Scenario: terminal_signal 调度与呈现
  Test: pending_req_fr_builtin_tools_terminal_signal
  假设 合约要求的行为已实现
  当 读取工具 terminal_signal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 execute

Scenario: terminal_signal 参数越界拒绝
  Test: pending_req_fr_builtin_tools_terminal_signal
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: terminal_signal 说明 section 注册
  Test: pending_req_fr_builtin_tools_terminal_signal_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_signal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: terminal_signal 参数 sessionId 形状
  Test: pending_req_fr_builtin_tools_terminal_signal_sessionid
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

Scenario: terminal_signal 参数 sessionId 约束
  Test: pending_req_fr_builtin_tools_terminal_signal_sessionid
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_signal 参数 signal 形状
  Test: pending_req_fr_builtin_tools_terminal_signal_signal
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [signal:string:req:enum[SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP]] 的名称与类型按目录行实现

Scenario: terminal_signal 参数 signal 约束
  Test: pending_req_fr_builtin_tools_terminal_signal_signal
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [signal:string:req:enum[SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_signal 行为 1
  Test: pending_req_fr_builtin_tools_terminal_signal_1
  假设 工具 terminal_signal 执行时
  当 达到触发条件时
  那么 shell 目标 SIGKILL 被拒用 terminal_close

Scenario: terminal_list 调度与呈现
  Test: pending_req_fr_builtin_tools_terminal_list
  假设 合约要求的行为已实现
  当 读取工具 terminal_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: terminal_list 参数越界拒绝
  Test: pending_req_fr_builtin_tools_terminal_list
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: terminal_list 说明 section 注册
  Test: pending_req_fr_builtin_tools_terminal_list_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: terminal_close 调度与呈现
  Test: pending_req_fr_builtin_tools_terminal_close
  假设 合约要求的行为已实现
  当 读取工具 terminal_close 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 delete

Scenario: terminal_close 参数越界拒绝
  Test: pending_req_fr_builtin_tools_terminal_close
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: terminal_close 说明 section 注册
  Test: pending_req_fr_builtin_tools_terminal_close_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_close 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: terminal_close 参数 sessionId 形状
  Test: pending_req_fr_builtin_tools_terminal_close_sessionid
  假设 工具 terminal_close 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

Scenario: terminal_close 参数 sessionId 约束
  Test: pending_req_fr_builtin_tools_terminal_close_sessionid
  假设 工具 terminal_close 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: terminal_close 行为 1
  Test: pending_req_fr_builtin_tools_terminal_close_1
  假设 工具 terminal_close 执行时
  当 达到触发条件时
  那么 等待 owned 进程树消亡

Scenario: skill 调度与呈现
  Test: pending_req_fr_builtin_tools_skill
  假设 合约要求的行为已实现
  当 读取工具 skill 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: skill 参数越界拒绝
  Test: pending_req_fr_builtin_tools_skill
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: skill 说明 section 注册
  Test: pending_req_fr_builtin_tools_skill_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 skill 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: skill 参数 name 形状
  Test: pending_req_fr_builtin_tools_skill_name
  假设 工具 skill 已注册
  当 校验 schema 时
  那么 参数行 [name:string:req（精确技能名）] 的名称与类型按目录行实现

Scenario: skill 参数 name 约束
  Test: pending_req_fr_builtin_tools_skill_name
  假设 工具 skill 已注册
  当 校验 schema 时
  那么 参数行 [name:string:req（精确技能名）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: load_workspace_dependencies 调度与呈现
  Test: pending_req_fr_builtin_tools_load_workspace_dependencies
  假设 合约要求的行为已实现
  当 读取工具 load_workspace_dependencies 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: load_workspace_dependencies 参数越界拒绝
  Test: pending_req_fr_builtin_tools_load_workspace_dependencies
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: load_workspace_dependencies 说明 section 注册
  Test: pending_req_fr_builtin_tools_load_workspace_dependencies_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 load_workspace_dependencies 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: load_workspace_dependencies 行为 1
  Test: pending_req_fr_builtin_tools_load_workspace_dependencies_1
  假设 工具 load_workspace_dependencies 执行时
  当 达到触发条件时
  那么 返回捆绑 Python+库路径与版本（numpy/pandas 等）；Node/pnpm 在 payload 提供时附上

Scenario: lsp 调度与呈现
  Test: pending_req_fr_builtin_tools_lsp
  假设 合约要求的行为已实现
  当 读取工具 lsp 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: lsp 参数越界拒绝
  Test: pending_req_fr_builtin_tools_lsp
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: lsp 说明 section 注册
  Test: pending_req_fr_builtin_tools_lsp_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 lsp 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: lsp 参数 operation 形状
  Test: pending_req_fr_builtin_tools_lsp_operation
  假设 工具 lsp 已注册
  当 校验 schema 时
  那么 参数行 [operation:string:req:enum[goToDefinition|findReferences|goToImplementation|hover]] 的名称与类型按目录行实现

Scenario: lsp 参数 operation 约束
  Test: pending_req_fr_builtin_tools_lsp_operation
  假设 工具 lsp 已注册
  当 校验 schema 时
  那么 参数行 [operation:string:req:enum[goToDefinition|findReferences|goToImplementation|hover]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: lsp 行为 1
  Test: pending_req_fr_builtin_tools_lsp_1
  假设 工具 lsp 执行时
  当 达到触发条件时
  那么 无 provider 返回结构化 LSP_UNAVAILABLE 不改 schema；config maxLocations 100、maxResultChars 16000、timeoutMs 60000（即 60000 归属 lsp）

Scenario: working_directory 调度与呈现
  Test: pending_req_fr_builtin_tools_working_directory
  假设 合约要求的行为已实现
  当 读取工具 working_directory 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: working_directory 参数越界拒绝
  Test: pending_req_fr_builtin_tools_working_directory
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: working_directory 说明 section 注册
  Test: pending_req_fr_builtin_tools_working_directory_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 working_directory 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: working_directory 参数 cd 形状
  Test: pending_req_fr_builtin_tools_working_directory_cd
  假设 工具 working_directory 已注册
  当 校验 schema 时
  那么 参数行 [cd:string:opt（省略读当前）] 的名称与类型按目录行实现

Scenario: working_directory 参数 cd 约束
  Test: pending_req_fr_builtin_tools_working_directory_cd
  假设 工具 working_directory 已注册
  当 校验 schema 时
  那么 参数行 [cd:string:opt（省略读当前）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: working_directory 行为 1
  Test: pending_req_fr_builtin_tools_working_directory_1
  假设 工具 working_directory 执行时
  当 达到触发条件时
  那么 shell 与运行进程保留自身目录；origin 元数据与权限根固定

Scenario: cordis_inspect_list 调度与呈现
  Test: pending_req_fr_builtin_tools_cordis_inspect_list
  假设 合约要求的行为已实现
  当 读取工具 cordis_inspect_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: cordis_inspect_list 参数越界拒绝
  Test: pending_req_fr_builtin_tools_cordis_inspect_list
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: cordis_inspect_list 说明 section 注册
  Test: pending_req_fr_builtin_tools_cordis_inspect_list_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 cordis_inspect_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: cordis_inspect_list 行为 1
  Test: pending_req_fr_builtin_tools_cordis_inspect_list_1
  假设 工具 cordis_inspect_list 执行时
  当 达到触发条件时
  那么 列全部 Inspect Provider；写/配插件前先调用

Scenario: cordis_inspect_query 调度与呈现
  Test: pending_req_fr_builtin_tools_cordis_inspect_query
  假设 合约要求的行为已实现
  当 读取工具 cordis_inspect_query 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

Scenario: cordis_inspect_query 参数越界拒绝
  Test: pending_req_fr_builtin_tools_cordis_inspect_query
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: cordis_inspect_query 说明 section 注册
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 cordis_inspect_query 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: cordis_inspect_query 参数 platform 形状
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_platform
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [platform:string:req:enum[host|client]] 的名称与类型按目录行实现

Scenario: cordis_inspect_query 参数 platform 约束
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_platform
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [platform:string:req:enum[host|client]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: cordis_inspect_query 参数 provider 形状
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_provider
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [provider:string:req（精确 ID）] 的名称与类型按目录行实现

Scenario: cordis_inspect_query 参数 provider 约束
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_provider
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [provider:string:req（精确 ID）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: cordis_inspect_query 参数 method 形状
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_method
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [method:string:req（manifest 精确方法）] 的名称与类型按目录行实现

Scenario: cordis_inspect_query 参数 method 约束
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_method
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [method:string:req（manifest 精确方法）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: cordis_inspect_query 参数 input 形状
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_input
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [input:any:opt（满足 input schema）] 的名称与类型按目录行实现

Scenario: cordis_inspect_query 参数 input 约束
  Test: pending_req_fr_builtin_tools_cordis_inspect_query_input
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [input:any:opt（满足 input schema）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 调度与呈现
  Test: pending_req_fr_builtin_tools_plugin_manager
  假设 合约要求的行为已实现
  当 读取工具 plugin_manager 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: plugin_manager 参数越界拒绝
  Test: pending_req_fr_builtin_tools_plugin_manager
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: plugin_manager 说明 section 注册
  Test: pending_req_fr_builtin_tools_plugin_manager_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 plugin_manager 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: plugin_manager 参数 action 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_action
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[list_plugins|list_bundles|set_plugin|set_bundle|install_bundle|remove_bundle|list_version_exemptions|set_version_exemption]] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 action 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_action
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[list_plugins|list_bundles|set_plugin|set_bundle|install_bundle|remove_bundle|list_version_exemptions|set_version_exemption]] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 target 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_target
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [target:opt] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 target 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_target
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [target:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 enabled 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_enabled
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [enabled:opt（set 必填）] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 enabled 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_enabled
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [enabled:opt（set 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 runtimeVersion 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_runtimeversion
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [runtimeVersion:opt（精确版本）] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 runtimeVersion 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_runtimeversion
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [runtimeVersion:opt（精确版本）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 acceptRisk 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_acceptrisk
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [acceptRisk:opt] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 acceptRisk 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_acceptrisk
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [acceptRisk:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 approvedBuilds 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_approvedbuilds
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [approvedBuilds:opt] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 approvedBuilds 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_approvedbuilds
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [approvedBuilds:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 registry 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_registry
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [registry:opt] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 registry 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_registry
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [registry:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 offset 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_offset
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 offset 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_offset
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 参数 limit 形状
  Test: pending_req_fr_builtin_tools_plugin_manager_limit
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [limit:opt:25（1..100）] 的名称与类型按目录行实现

Scenario: plugin_manager 参数 limit 约束
  Test: pending_req_fr_builtin_tools_plugin_manager_limit
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [limit:opt:25（1..100）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: plugin_manager 行为 1
  Test: pending_req_fr_builtin_tools_plugin_manager_1
  假设 工具 plugin_manager 执行时
  当 达到触发条件时
  那么 每动作需 danger-full-access 或本次批准；安装可执行允许的 build 脚本

Scenario: subagent 调度与呈现
  Test: pending_req_fr_builtin_tools_subagent
  假设 合约要求的行为已实现
  当 读取工具 subagent 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: subagent 参数越界拒绝
  Test: pending_req_fr_builtin_tools_subagent
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: subagent 说明 section 注册
  Test: pending_req_fr_builtin_tools_subagent_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 subagent 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: subagent 参数 description 形状
  Test: pending_req_fr_builtin_tools_subagent_description
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（3-5 词）] 的名称与类型按目录行实现

Scenario: subagent 参数 description 约束
  Test: pending_req_fr_builtin_tools_subagent_description
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（3-5 词）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: subagent 参数 prompt 形状
  Test: pending_req_fr_builtin_tools_subagent_prompt
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req（自包含，child 不共享对话）] 的名称与类型按目录行实现

Scenario: subagent 参数 prompt 约束
  Test: pending_req_fr_builtin_tools_subagent_prompt
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req（自包含，child 不共享对话）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: subagent 参数 cwd 形状
  Test: pending_req_fr_builtin_tools_subagent_cwd
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（相对按当前，省略继承；双方后续变更互不影响）] 的名称与类型按目录行实现

Scenario: subagent 参数 cwd 约束
  Test: pending_req_fr_builtin_tools_subagent_cwd
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（相对按当前，省略继承；双方后续变更互不影响）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: subagent 行为 1
  Test: pending_req_fr_builtin_tools_subagent_1
  假设 工具 subagent 执行时
  当 达到触发条件时
  那么 立即返回 child id，完成通知；config provider 必填、maxDepth（0=禁委托或 provider-managed；省略读 Host 默认 1）、agentPreset/allowedAgentPresets（非空才暴露 agent_preset）

Scenario: interrupt_agent 调度与呈现
  Test: pending_req_fr_builtin_tools_interrupt_agent
  假设 合约要求的行为已实现
  当 读取工具 interrupt_agent 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: interrupt_agent 参数越界拒绝
  Test: pending_req_fr_builtin_tools_interrupt_agent
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: interrupt_agent 说明 section 注册
  Test: pending_req_fr_builtin_tools_interrupt_agent_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 interrupt_agent 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: interrupt_agent 参数 agent_id 形状
  Test: pending_req_fr_builtin_tools_interrupt_agent_agent_id
  假设 工具 interrupt_agent 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req（直接子代或更深后代）] 的名称与类型按目录行实现

Scenario: interrupt_agent 参数 agent_id 约束
  Test: pending_req_fr_builtin_tools_interrupt_agent_agent_id
  假设 工具 interrupt_agent 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req（直接子代或更深后代）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: interrupt_agent 行为 1
  Test: pending_req_fr_builtin_tools_interrupt_agent_1
  假设 工具 interrupt_agent 执行时
  当 达到触发条件时
  那么 请求停止不等待；外部执行永久停止；其启动的 subagent 继续跑

Scenario: list_agents 调度与呈现
  Test: pending_req_fr_builtin_tools_list_agents
  假设 合约要求的行为已实现
  当 读取工具 list_agents 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: list_agents 参数越界拒绝
  Test: pending_req_fr_builtin_tools_list_agents
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: list_agents 说明 section 注册
  Test: pending_req_fr_builtin_tools_list_agents_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 list_agents 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: list_agents 参数 scope 形状
  Test: pending_req_fr_builtin_tools_list_agents_scope
  假设 工具 list_agents 已注册
  当 校验 schema 时
  那么 参数行 [scope:opt:enum[children|descendants]（descendants 深>1 仅可 interrupt）] 的名称与类型按目录行实现

Scenario: list_agents 参数 scope 约束
  Test: pending_req_fr_builtin_tools_list_agents_scope
  假设 工具 list_agents 已注册
  当 校验 schema 时
  那么 参数行 [scope:opt:enum[children|descendants]（descendants 深>1 仅可 interrupt）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: send_message 调度与呈现
  Test: pending_req_fr_builtin_tools_send_message
  假设 合约要求的行为已实现
  当 读取工具 send_message 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: send_message 参数越界拒绝
  Test: pending_req_fr_builtin_tools_send_message
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: send_message 说明 section 注册
  Test: pending_req_fr_builtin_tools_send_message_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 send_message 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: send_message 参数 agent_id 形状
  Test: pending_req_fr_builtin_tools_send_message_agent_id
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req] 的名称与类型按目录行实现

Scenario: send_message 参数 agent_id 约束
  Test: pending_req_fr_builtin_tools_send_message_agent_id
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: send_message 参数 message 形状
  Test: pending_req_fr_builtin_tools_send_message_message
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [message:string:req] 的名称与类型按目录行实现

Scenario: send_message 参数 message 约束
  Test: pending_req_fr_builtin_tools_send_message_message
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [message:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: send_message 行为 1
  Test: pending_req_fr_builtin_tools_send_message_1
  假设 工具 send_message 执行时
  当 达到触发条件时
  那么 工作中者最近 step 收到，空闲者开新轮；返回送达确认非答案

Scenario: list_subagent_models 调度与呈现
  Test: pending_req_fr_builtin_tools_list_subagent_models
  假设 合约要求的行为已实现
  当 读取工具 list_subagent_models 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

Scenario: list_subagent_models 参数越界拒绝
  Test: pending_req_fr_builtin_tools_list_subagent_models
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

Scenario: list_subagent_models 说明 section 注册
  Test: pending_req_fr_builtin_tools_list_subagent_models_section
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 list_subagent_models 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

Scenario: list_subagent_models 参数 provider 形状
  Test: pending_req_fr_builtin_tools_list_subagent_models_provider
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [provider:opt] 的名称与类型按目录行实现

Scenario: list_subagent_models 参数 provider 约束
  Test: pending_req_fr_builtin_tools_list_subagent_models_provider
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [provider:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: list_subagent_models 参数 model 形状
  Test: pending_req_fr_builtin_tools_list_subagent_models_model
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [model:opt（需 provider）] 的名称与类型按目录行实现

Scenario: list_subagent_models 参数 model 约束
  Test: pending_req_fr_builtin_tools_list_subagent_models_model
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [model:opt（需 provider）] 的 required、默认值与枚举约束按目录行实现并冻结对照

Scenario: list_subagent_models 行为 1
  Test: pending_req_fr_builtin_tools_list_subagent_models_1
  假设 工具 list_subagent_models 执行时
  当 达到触发条件时
  那么 目录仅 advisory

## Questions

- Source trace: doc/prd.md#17: 内置工具清单与关键语义；参数级 schema 权威来源 docs/tool-catalog.md, doc/prd.md#22: ripgrep 分发对策, confidence: high（清单级；参数级以 catalog 为权威）
- Replace pending test selectors with real test names before lifecycle verification.
