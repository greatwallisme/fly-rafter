# dsh 权威目录结构化数据（场景生成器数据源）

| 项 | 值 |
|---|---|
| 来源 | dsh docs/tool-catalog.md（参数表逐字）+ docs/config-catalog.md（生成+运行时交叉校验）；executionMode/presentation 取自源码（catalog 无此二字段）：默认 exclusive，parallel 仅 isConcurrencySafe→true（fail-closed），presentation 未声明回退 generic |
| 工具范围 | catalog 共 73 工具块（60 模型可见名）；fly-rafter 只收录 PRD 17 清单，ralph/workflow/team 九件/stagehand/create_worktree/present/plugin_market 属排除清单（SCOPE 覆盖） |

## 分层默认值裁定（bash timeout）

- bash-local schema 缺省 timeoutMs=120000、maxTimeoutMs=600000（packages/shell/bash-local/src/index.ts:102-103）
- base patch bash-sandbox 行覆盖 timeoutMs=60000（cordis.patch.yml:253）→ 默认装配生效值 60000
- PRD 6.4「60000」= 装配生效值；schema 缺省另记。两层并存，均须在 fr 复现（schema 缺省 + patch 覆盖）

## TOOLS（收录工具参数与行为）

### bash | exclusive | terminal
- params: description:string:req（主动语态 5-10 词，置于 command 前）; command:string:req（bash -c 命令）; timeoutMs:number:opt（执行器施加默认与上限；到期转后台 job 而非杀死）; workdir:string:opt（默认 session workspace）; run_in_background:boolean:opt（无 job registry 或 enableRunInBackground:false 时不注册）
- behavior: 每次调用全新 shell，勿用 cd 传 workdir；DSH_* 暴露 harness 事实；长输出截尾、完整输出落盘报路径；删除/移动前校验解析后绝对路径；未设变量展开为空串用 ${VAR:?} 防护；沙箱拒绝报 [sandbox: file access denied under <mode> mode] 为策略性拒绝勿换法重试

### pwsh | exclusive | terminal
- params: 同 bash 五参（约束逐字同）
- behavior: pwsh -Command 方言，每次新进程无持久 PTY；C:\ 原生路径与 $env:NAME；Windows 强杀 [exit code: 1] 视为中断；勿赋值 $HOME 等自动变量；mirror bash 但无沙箱控制

### persistent bash | exclusive | terminal
- params: command:string:req
- behavior: owner 隔离持久 PTY，cwd/env 跨调用保持；config backendType 默认 shell、timeoutMs 300000、maxOutputChars 16000

### persistent pwsh | exclusive | terminal
- 同 persistent bash 的 Windows 对应物，config 同上

### read | parallel | read
- params: file_path:string:req; offset:number:opt:1（1-based 起始行）; limit:number:opt:2000（最大行数）
- behavior: 返回行号内容；config readLimit（默认兼上限）/readMaxLineLength/readMaxBytes/readStreamMinSize（超限转流）；presence/absence 发 fs/observed

### read_image | parallel | generic
- params: file_path:string:req
- behavior: PNG/JPEG/WebP/GIF 返回图像本体，大图自动缩；无 ctx.attachments 不注册；仅精确路由模型声明 image input 才执行否则拒绝

### write | exclusive | diff
- params: file_path:string:req（先于 content）; content:string:req（完整 UTF-8）
- behavior: 创建或整文件替换；发 fs/write-intent；read-before-write 由 fs-observation-policy 施加

### edit | exclusive | diff
- params: file_path:string:req（先于 old/new）; old_string:string:req; new_string:string:req（空串即删除）; replace_all:boolean:opt:false（false 时 old_string 须恰好出现一次）
- behavior: UTF-8 字面替换；发 fs/edit-intent；read-before-write 同 write

### glob | exclusive | search
- params: pattern:string:req（无分隔符按任意深度 basename 匹配，带分隔符锚定深度）; path:string:opt（默认 session workspace）
- behavior: 打包 ripgrep 经 ctx.subprocess 前台运行（永不后台 job、无 host rg 依赖、无 shell 层）；返回 ≤100 路径按 mtime 序；超限按 sampleOverCapGlobResults 顶层采样并报完整列表保存位置（spill 落盘）

### grep | exclusive | search
- params: pattern:string:req（ripgrep 正则）; path:string:opt; include:string:opt（单 glob，不支持否定）
- behavior: 匹配行+行号按文件分组 ≤250 匹配；超限报完整列表位置；config globMaxResults/grepMaxMatches/grepMaxLineBytes/searchMetaMaxBytes/rawOutputMaxBytes（溢出 SEARCH_RAW_OUTPUT_OVERFLOW）/graceMs/stderrMaxBytes/timeoutMs

### str_replace_editor | exclusive | diff(str_replace/create)+generic(view)
- params: command:string:req:enum[view|create|str_replace|insert]; path:string:req（绝对路径）; file_text:string|null:opt（create 必填，已存在不可 create）; insert_line:integer|null:opt（insert 必填）; new_str:string|null:opt（str_replace 可选=删除，insert 必填）; old_str:string|null:opt（str_replace 必填，须唯一匹配否则不替换）; view_range:array<integer>|null:opt（1-based，[start,-1] 到末尾）
- behavior: view=cat -n；目录 view 列非隐藏两层；长输出截断标 <response clipped>；未用命令的 null 占位视为省略

### web_search | parallel | search+web
- params: queries:array<string>:req（1-4 条，结果合并）
- behavior: 可选摘要答案+源 URL 列表；config searchTimeoutMs 30000、searchMaxQueries/searchMaxResults 上限；后端 deepseek/exa/perplexity

### web_fetch | parallel | fetch+web
- params: url:string:req（HTTP(S)）
- behavior: 解码为文本；config fetchTimeoutMs 30000、fetchMaxOutputChars 200000、search/fetch 注册开关默认 true

### todo_write | exclusive | other
- params: todos:array<object>:req（完整表替换）; todos[].content:string:req; todos[].status:string:req:enum[pending|in_progress|completed]
- behavior: session 持有状态；并行工作可多个 in_progress（allowParallelInProgress:true），false 则恰一活跃且多标拒绝

### create_goal | exclusive | other
- params: objective:string:req（直接人类请求推断的长时目标）; max_goal_rounds:number:opt（正安全整数）
- behavior: 跨自动续轮；仅长时目标
### get_goal | exclusive | read
- params: none; behavior: 读当前 goal 含 id/revision
### update_goal | exclusive | generic
- params: goal_id:string:req; revision:number:req（精确正 revision）; action:string:req:enum[edit|pause|resume|complete|blocked]; objective:string:opt（仅 edit）; max_goal_rounds:number:opt（仅 edit）; blocked_reason:string:opt（仅 blocked 必填）
- behavior: edit/pause/resume 需直接人类 root 权威；blocked 在最小轮前被拒（blockedAfterConsecutiveRounds 默认 3）

### job_kill | exclusive | execute
- params: job_id:string:req; reason:string:opt（入日志并转发）
### job_list | exclusive | read
- params: none; behavior: 列 running+finished 的 id/kind/status
### job_output | exclusive | read
- params: job_id:string:req; wait:boolean:opt:false; timeout_ms:number:opt（默认 30s，钳至 maxWaitTimeoutMs 10min）
- behavior: 流式增量读取；wait 超时留 job 运行

### schedule_create | exclusive | other
- params: prompt:string:req; title:string:req（≤120 字符规则）; after_seconds/every_seconds（≥60 对齐）/daily（time+time_zone）/weekly（+weekdays ISO 周一1）/cron（五字段 Vixie+time_zone）/at（RFC3339 或 date+time+time_zone）:opt——六计时参数恰选一
- behavior: 时区 UTC 或 IANA；不存在本地时刻跳过、重复取较早；宕机后循环提醒只补最近一次；管理走 storage domain
### schedule_delete | exclusive
- params: id:string:req; behavior: 活动/非活动均可删，不撤回已入队
### schedule_list | exclusive | read
- params: none; behavior: 列当前 session 活动提醒
### schedule_update | exclusive
- params: id:string:req; title/prompt/计时参数:opt（至多一个，省略保持存储值）
- behavior: 原地改保留 id；改相对延迟须新建；every_seconds 改动按保存时刻重对齐

### ask_user_question | exclusive | generic
- params: questions:array<object>:req（[].id:req 稳定 id、[].question:req、[].header:opt、[].options:opt（label 必填+description；推荐首位加 (Recommended)）、[].multi_select:opt:false）
- behavior: 默认阻塞等回答；config mode:timed 启用超时+pending result（timeout 默认 120s，-1 无限阻塞）

### exit_plan_mode | exclusive | generic
- params: plan:string:req（完整 markdown，# 标题开头）
- behavior: schema 常驻防 churn；plan 外 execute 拒绝；plan 内经 user-questions 呈批 approve/继续规划；批准在 step 边界记录 inactive

### run_code | exclusive | generic（PTC）
- params: code:string:req（TypeScript 程序）; description:string:req（5-10 词）; timeoutMs:number:opt（正数，受部署上限封顶）; sandbox_permissions:string:opt:enum[workspace-write|danger-full-access]; justification:string:opt（用户当前请求语言）
- behavior: mode:ptc 保留传输不可过滤；子调用经完整 guarded pipeline；并发安全子调用重叠至 maxParallelSubCalls（默认 10，1=串行）；图片型子工具结果 run 后附加

### session_search | exclusive | search
- params: query:string:req; session_ids/created_at_from/to/parent_session_ids/include_root_sessions:opt; availability:enum[live|persisted]:opt（至少一选）; event_seq_from/to、event_time_from/to、event_types、event_surfaces:enum[current|shadowed|log-only]:opt
- behavior: 跨 workspace 搜先前会话每会话回最强匹配；config maxSearchResults 100、searchTimeoutMs 30000
### session_trace | parallel
- params: session_id:opt（省略当前）; behavior: 读授权谱系
### session_event_read | parallel
- params: seq:integer:req; session_id:opt; before/after:integer:opt
### session_event_search | exclusive | search
- params: query:string:req; session_id/seq_from/seq_to/time_from/time_to/event_types/surfaces:opt
- behavior: 排除执行本调用的 step
### session_event_trace | parallel
- params: seq:integer:req; session_id:opt; behavior: 读直接替换与关系

### terminal_open | exclusive | execute
- params: type:string:req（通常 shell）; name:opt; cwd:opt（非绝对按 Session 目录解析）
### terminal_read | exclusive
- params: sessionId:req; offset:opt:0; count:opt:500（后端封顶）
### terminal_send | exclusive
- params: sessionId:req; text:string:req; submit:boolean:opt:true; run_in_background:opt（立即返回 job id）
- behavior: 默认等 prompt/静默/超时/退出；后台经 ctx.jobs
### terminal_signal | exclusive | execute
- params: sessionId:req; signal:string:req:enum[SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP]
- behavior: shell 目标 SIGKILL 被拒用 terminal_close
### terminal_list | exclusive | read
- params: none
### terminal_close | exclusive | delete
- params: sessionId:req; behavior: 等待 owned 进程树消亡

### skill | exclusive | read
- params: name:string:req（精确技能名）
### load_workspace_dependencies | exclusive | read
- params: none; behavior: 返回捆绑 Python+库路径与版本（numpy/pandas 等）；Node/pnpm 在 payload 提供时附上

### lsp | exclusive | generic
- params: operation:string:req:enum[goToDefinition|findReferences|goToImplementation|hover]; file_path/line（1-based）/character（1-based UTF-16）:req
- behavior: 无 provider 返回结构化 LSP_UNAVAILABLE 不改 schema；config maxLocations 100、maxResultChars 16000、timeoutMs 60000（即 60000 归属 lsp）

### working_directory | exclusive | generic
- params: cd:string:opt（省略读当前）
- behavior: shell 与运行进程保留自身目录；origin 元数据与权限根固定

### cordis_inspect_list | exclusive | generic
- params: none; behavior: 列全部 Inspect Provider；写/配插件前先调用
### cordis_inspect_query | exclusive | read
- params: platform:string:req:enum[host|client]; provider:string:req（精确 ID）; method:string:req（manifest 精确方法）; input:any:opt（满足 input schema）
### plugin_manager | exclusive | generic
- params: action:string:req:enum[list_plugins|list_bundles|set_plugin|set_bundle|install_bundle|remove_bundle|list_version_exemptions|set_version_exemption]; target:opt; enabled:opt（set 必填）; runtimeVersion:opt（精确版本）; acceptRisk:opt; approvedBuilds:opt; registry:opt; offset:opt:0; limit:opt:25（1..100）
- behavior: 每动作需 danger-full-access 或本次批准；安装可执行允许的 build 脚本
### subagent | parallel | generic
- params: description:string:req（3-5 词）; prompt:string:req（自包含，child 不共享对话）; cwd:opt（相对按当前，省略继承；双方后续变更互不影响）
- behavior: 立即返回 child id，完成通知；config provider 必填、maxDepth（0=禁委托或 provider-managed；省略读 Host 默认 1）、agentPreset/allowedAgentPresets（非空才暴露 agent_preset）
### interrupt_agent | exclusive
- params: agent_id:string:req（直接子代或更深后代）; behavior: 请求停止不等待；外部执行永久停止；其启动的 subagent 继续跑
### list_agents | exclusive
- params: scope:opt:enum[children|descendants]（descendants 深>1 仅可 interrupt）
### send_message | exclusive
- params: agent_id:string:req; message:string:req; behavior: 工作中者最近 step 收到，空闲者开新轮；返回送达确认非答案
### list_subagent_models | exclusive
- params: provider:opt; model:opt（需 provider）; behavior: 目录仅 advisory

## CONFIG（fly-rafter 相关插件；格式 键:类型:默认:约束）

### dsh-agent-loop
- maxParallelToolCalls:Volatile<number>:-（省略用内置 DEFAULT 10）; agents:启动时创建/恢复的 agents
### dsh-agent-instructions
- projectRootMarkers:string[]（.git 等）; maxBytes:number（非正/非有限禁用加载）; maxSourceBytes:number（单文件上限超限忽略）; instructionFileCandidates/localInstructionFileCandidates:string[]
### dsh-agent-default-model
- provider/model/reasoningEffort:均 Volatile
### dsh-agent-tool-presentation / dsh-tools
- mode:'native'|'ptc':tools 默认 native；ptc 需 ctx.ptcRuntime 且语言有 SDK renderer 否则组装失败; maxParallelSubCalls:10（正整数，1=串行）
### dsh-attachment-local
- dshHome:-($DSH_HOME→~/.dsh); maxImageBytes:20MiB; maxImagesPerMessage:20; maxMessageImageBytes:200MiB; maxImagePixels:64000000; maxImageDimension:8192px
### dsh-bash-local（执行器默认值真源）
- cwd:Volatile:process.cwd(); timeoutMs:Volatile:120000; maxTimeoutMs:Volatile:600000; maxOutputBytes:Volatile:64000（溢出落临时文件）; maxSpillBytes; graceMs
### dsh-bash-sandbox / dsh-pwsh-sandbox / dsh-fs-sandbox
- 无自有键（bash-sandbox Config=bash-local 原样；base patch 在此行覆盖 timeoutMs=60000）
### dsh-compaction-basic
- auto:true; thresholdRatio:0.8; headroomTokens:65536; retainRatio:0.16（与 retainTokens 互斥）; summarizationProvider/Model（须成对否则继承会话目标）; maxTokens（默认 headroomTokens）; compactionRetries:1; maxOverflowRetries:1（0 禁恢复）; modelPolicies（精确覆盖，重复目标加载失败）
### dsh-compaction-tool-result-pruner
- thresholdChars:8192（Unicode 码点）; headChars:4096; tailChars:1024
### dsh-credentials-local
- path（默认 home 下 .credentials.yaml）; dshHome; watch:true; debounceMs:100
### dsh-goal
- defaultMaxGoalRounds
### dsh-headless
- task:-（省略来自 stdin）; sessionId:-（无存储会话则失败）; json:false
### dsh-hmr
- base/root:string[]/debounce/ignored（fr 裁剪 HMR，仅记录）
### dsh-jobs-local
- maxConcurrentJobsPerOwner:10; retainBytes:262144; settledRetainBytes:16384; pumpPollMs:150
### dsh-llm-deepseek-api-key
- apiKeyEnv:Volatile:'DEEPSEEK_API_KEY'（每请求解析）
### dsh-llm-pi-ai
- providers:Volatile:Record（空=休眠）；per-provider apiKeyEnv/api/baseURL/models/compat/defaultContextWindow:262144/defaultMaxTokens:32768/defaultInput:['text']/headers/retryPolicy（normal 5 次）等
### dsh-lsp-stdio
- servers:Record 非空; per-server command（PATH 解析）/extensionToLanguage 必填/args/env/maxMessageBytes:16000000/maxStderrBytes:1000000/maxDocumentBytes:4000000/shutdownTimeoutMs:5000/killGraceMs:2000
### dsh-mcp-client（stdio 与 streamable-http 两变体）
- serverName:-（[A-Za-z0-9_-]{1,32} 且唯一，工具名 mcp__<server>__<raw>）; toolCallTimeoutMs; failOnStartupError; maxInstructionBytes:32768; reconnect（initialDelayMs 500 翻倍/maxDelayMs 30000/maxAttempts 10）; stdio 必填 command/args/env/cwd；http 必填 url/headers
### dsh-permission-presets
- presets:Record（默认 workspace-write=workspace-write+ask、danger-full-access=danger+never；custom/auto 保留）; defaultPreset:Volatile
### dsh-persona
- prefix/suffix/complete/includeRuntimeContext
### dsh-plan-mode
- section:string（plan 活动期渲染 plan:policy 段）
### dsh-plugin-manager
- pnpmCommand/outputBytes/lockWaitMs/inspectTimeoutMs/githubConnectionTimeoutMs/idleTimeoutMs/registry/fallbackRegistries
### dsh-ptc-runtime-node
- launch:node-script|embedded; executionMode:'per-call'|'persistent-in-memory' 默认 per-call; timeoutMs/maxTimeoutMs; maxOutputBytes; maxOldGenerationSizeMb; maxMessageBytes/maxPendingCalls/graceMs
### dsh-pwsh-local
- 同 bash-local 六键 + pwshPath（省略按知名位置探测 PS7→PATH→PS5.1）
### dsh-repeat-tool-reminder
- thresholds:[3,5,8]; include/exclude:pattern; argumentsPreviewChars:500
### dsh-sandbox-local
- runnerCommand/runnerFailureSignatures/probeTimeoutMs
### dsh-sandbox-policy
- mode:'read-only'（会话起始模式，DSH_PERMISSION_MODE/FR 覆盖）; workspaceRoot:process.cwd()
### dsh-schedule
- deliveryHistoryDays:30; deliveryHistoryRecords:200
### dsh-sdk-jsonrpc-server
- maxTokensAsSuccess:-（默认 false）; input/output/exit（runtime-only）
### dsh-session-persistence-jsonl
- root:-必填（无默认，防 cwd 漂移）; compression（默认带校验和 Zstandard 帧）; listConcurrency:3
### dsh-session-projection-cache
- writeEveryEvents/writeIntervalMs
### dsh-session-query-sqlite
- path（:memory: 支持）; openAt（默认 startup，never=禁全文搜索）; journalMode:'wal'; defaultLimit:20; maxLimit:100; snippetChars:240; persistedReadConcurrency:4; preparedSessionCacheSize:5
### dsh-shell-env
- dshHome（暴露 DSH_HOME）
### dsh-skill-filesystem
- providerName:'filesystem'; includeDefaultRoots; agentsHome; customSkillDirs; watch 系列; bundledSkillDir
### dsh-spill-local
- root（省略=OS temp 0700 懒创建）; cleanupPeriodDays:30（0 禁清理）
### dsh-spill-policy
- maxInlineTokens:-（省略禁保留）
### dsh-storage-domain / dsh-storage-json
- backend 必填（默认后端）+routes; root（每 unit 一个 json）
### dsh-subagent
- maxActiveSubagents:Volatile:8; maxDepth:Volatile:1
### dsh-subagent-acp
- providerName:'acp'; command/args; permission 默认 reject; env; disposeGraceMs
### dsh-system-prompt
- includeHarnessIdentity:true; includeRuntimeContext:true; personaPrefix/personaSuffix; toolOrder（REST 恰一次；非法 load 失败，未知名组装失败；省略字典序）
### dsh-terminal-bash
- backendType:'shell'; shellDialect:'bash'; shellPath/shellArgs（bash --noprofile --norc -i）; rows/cols/scrollback 系列/timeoutMs/disposeGraceMs
### dsh-time-context
- timeZone; refreshIntervalMs:600000（0=每合格 step 注入）
### dsh-tool-ask-user
- mode:'legacy'|'timed'（默认阻塞 legacy）; timeout:120s
### dsh-tool-bash / dsh-tool-pwsh
- enableRunInBackground:true; promoteOnTimeout:true（超时转后台 job 不杀；仅后台可用时生效）
### dsh-tool-bash-persistent / dsh-tool-pwsh-persistent
- backendType:'shell'; timeoutMs:300000; maxOutputChars:16000; description
### dsh-tool-fs
- read/readImage/write/edit:boolean:true; readLimit; readMaxLineLength; readMaxBytes; readStreamMinSize
### dsh-tool-fs-search
- sampleOverCapGlobResults:必填; globMaxResults/grepMaxMatches/grepMaxLineBytes; searchMetaMaxBytes; rawOutputMaxBytes（溢出 SEARCH_RAW_OUTPUT_OVERFLOW）; graceMs; stderrMaxBytes; timeoutMs
### dsh-tool-goal
- blockedAfterConsecutiveRounds（默认 3）
### dsh-tool-jobs
- waitTimeoutMs:30s; maxWaitTimeoutMs:10min; completionDelivery:'wakeup'; maxConsecutiveWakes
### dsh-tool-lsp
- maxLocations:100; maxResultChars:16000; timeoutMs:60000
### dsh-tool-present
- maxFiles
### dsh-tool-session-query
- maxSearchResults:100; searchTimeoutMs:30000
### dsh-tool-str-replace-editor
- maxOutputChars:16000; description
### dsh-tool-subagent
- provider:必填; toolName:'subagent'（实例名互异）; modelSelectionSettings; maxDepth（0 禁委托或 provider-managed；省略读 Host 默认 1）; agentPreset/allowedAgentPresets
### dsh-tool-terminal
- enableRunInBackground:true; maxResultBytes
### dsh-tool-todo
- allowParallelInProgress:必填部署选择（true=多 in_progress；false=恰一且多标拒绝）
### dsh-tool-web
- search:true; fetch:true; searchMaxResults/searchMaxQueries; fetchTimeoutMs:30000; searchTimeoutMs:30000; fetchMaxOutputChars:200000
### dsh-web-deepseek-api / dsh-web-fetch-http
- apiKey 优先 apiKeyEnv:'DEEPSEEK_API_KEY'; baseURL（追加 /search /fetch）; count:10（1..50）; sendOriginalQuestion:true; maxResponseBytes/maxBodyChars/timeoutMs/maxRedirects/userAgent
### dsh-web-search-exa / dsh-web-search-perplexity
- apiKey（$EXA_API_KEY 兜底空=不可用）/searchType auto; $PERPLEXITY_API_KEY/model sonar/maxTokens 1024/searchRecency
### dsh-working-directory
- defaultDirectory
### dsh-session-title（三变体）
- 无 config 键（title 行为参数见 PRD 6.4：回退 5 词 40B 上限 80；LLM targetWords 5 CJK 10 maxInput 4096 timeout 60s maxOutputTokens 4096）
### 零配置插件
- agent、authorization、user-approval、session、session-title 三变体、commands、command-feedback、command-goal、command-compact、goal、plan-mode（config 仅 section 段）、subagent-spawn/fork-in-process、tool-subagent 系列、tools、system-prompt（见上）、token-meter、user-questions（ask_user_question 行为宿主）、web、working-directory 等
