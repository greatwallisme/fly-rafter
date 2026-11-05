spec: task
name: "headless 入口"
tags: [requirements, generated-draft, headless, entry, cli]
satisfies: [REQ-FR-HEADLESS]
---

## Intent

headless 是第一个交付入口（M1）：一次性任务、`--json` 事件流、审批 fail-closed、`--session-id` 续用。装配不挂任何 Host/HTTP/Web 运行时，禁用交互式凭证插件，无 UI answerer。宿主壳（fr bin）为装配器+事件泵+入口分发：headless 组合经 fr-config 编译为 loader 文档、以真实 cdylib 工件经 dougong loader 装载走通，信号面与启动诊断落盘构成 boot 链的失败面。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- CLI MUST 为 `fr headless [TASK] [--session-id <id>] [--json] [--help]`；TASK 缺省或为裸 `-` 时从 stdin 读一次整段任务文本；无 TASK 且 stdin 为 TTY MUST 返回 usage error（exit 非 0）；给了 TASK 就不读 stdin；不解析任何交互命令。
- 默认输出 MUST：仅最终答案文本到 stdout；provider reasoning delta 实时流到 stderr（`fr: reasoning:` 标题）；错误 `fr: <code>: <message>` 到 stderr；exit 0 = 完成，1 = 中止或出错。
- `--json` 模式 MUST：stdout 为换行分隔 JSON 事件流；首事件 `{"type":"session","sessionId":...}`，末事件 `{"type":"final", ...}`（无损最终答案）；中间事件 status / text / thinking / tool_call / tool_result。
- `--json` 的 text/thinking MUST 从已提交 assistant message 投影（重试/丢弃 attempt 不出现；按 step 提交而非逐 token）。
- `--json` 截断规则 MUST：除 final 外所有字符串/对象值封顶 8 KiB（标 truncated:true），单行封顶 32 KiB，嵌套 64 层截断；运行错误输出 error 事件且无 final。
- 审批 MUST：无 answerer，一切 ask fail-closed 'unavailable'（模型收到拒绝结果）；`FR_PERMISSION_MODE=danger-full-access` 时策略 never，工具直接放行。
- `--session-id <id>` MUST 采纳已持久化会话继续（不存在的 id 直接失败；拒绝采纳子代理/fork 会话与 agent preset 不匹配的会话）；默认新 `session-<uuid>`。
- 装配 MUST 不挂任何 Host/HTTP/Web 运行时；禁用交互式凭证插件；无 UI answerer。
- 续用与输出硬细则 MUST：连字符任务参数必须唯一；任务参数非空但 trim 后为空报错；--session-id trim 后判空但原串交付（空白属身份）；--json 下语法错误也发 error 事件；采纳四拒（preset 不匹配、子代理与 fork、无 cwd、cwd 不一致）加 live Agent 拒绝加 resume 双重复查；final 文本 = durable log 最后一段非空 assistant text、exit reason 取 turn/end reason；等待托管子孙结束（子代理收尾提前唤醒时以 seq 未变且无存活子判定抑制）；--json 首事件含 cwd；字符串与 key 均封顶 8KiB、行限 32KiB 二级降级梯；usage 跨 attempt 累加且任一 attempt 无样本则整 step 省略；tool_result 仅 surfaceOp 为 append 时投影（dsh startup.ts:83-113、index.ts:73-99,212-292,375-386、json-stream.ts:16-66,132-149,266-303）。
- 宿主壳三件套 MUST（PRD 4.3、决策 16/27）：`fr` bin = 装配器（fr-config 编译装配文档，入口默认 bundle 嵌入常量走同一解析路径）+ 事件泵（emit 事件按订阅插件 descriptor 的 listen 表送达 on_event）+ 入口分发；装载文档每槽 `{name, source{kind}, config, isolate}`，第一方槽一律 dylib 源，builtin 源在 fr 装配中从不使用（决策 16）。
- dylib 装载清单 MUST 覆盖 headless 组合所需的全部第一方插件 cdylib 工件（现 20 crate 均为 cdylib+rlib 双输出，已实勘）；fr-llm-retry 与 fr-working-directory 两行占位待各自切片兑现（2026-11-04 裁决 2B）。
- cmdline/appExit MUST 为宿主值注入形态（决策 47 5A，2026-11-04 执法点核对后修订）：宿主壳解析根 argv，入口事实（app argv 快照）烘进装载文档行 config；读取轨道经入口插件自 provide 的 headless-startup 服务在 boot 窗由宿主中继同一快照（dougong 执法事实：dylib 激活不收槽位 config——`DynPlugin::apply` 丢弃 `_config`（dougong-dylib lib.rs:1676）、`provide` 必须 fiber 绑定故宿主不能 provide、决策 16 禁 builtin 源插件；5A 原文「dougong 等价面开工时按其契约核对后定案细节」兑现）；宿主壳持有信号注册与有界退出通道，插件不直接退出进程。
- fr-headless 最小入口插件脚手架随 18a 落地（2026-11-04 裁决 Q3A）：真实 cdylib 工件（descriptor provide "headless-startup"、inject 空、listen 空），激活持 boot 窗中继快照并经服务面读回；startup 参数解析/runner 驱动/json 投影等行为面随后续切片扩同一 crate。新建 crate 须改根 Cargo.toml members（Allowed 已扩该文件，仅限 members 数组增行）。
- `fr headless` CLI 分发面随 18a 挂起（2026-11-04 裁决 Q2A）：走完整 boot 链（装配→装载→join 收敛→事件泵→信号面），入口插件激活持事实后进程等信号（SIGINT 130/SIGTERM 0）；runner 语义随 18b+ 落。launcher 全局旗标 `--profile`/`--from-default-profile`/`--patch` 先于首个非旗标 token（dsh args.ts:166-174 同构），`fr headless [app...]` 为 profile 缩写展开（dsh args.ts:205-208 同构）。
- 六枚集成测试的 cdylib 工件由四关门流程前置 `cargo build --workspace` 供给（2026-11-04 裁决 Q1A：`cargo test` 不产 cdylib 工件——实测；dougong loader_demo 判例同形）；测试对缺失工件 fail-loud 并给指引，绝不静默跳过。
- 信号面 MUST：SIGINT 映射退出码 130、SIGTERM 映射退出码 0，宿主壳先广播 shutdown 再有界 join 插件自有线程（teardown join 纪律，dsh profile-boot.ts:307-308 同码）。
- 启动诊断 MUST：boot 期致命错误 stderr 单行摘要 + 完整报告落盘 `$FR_HOME/logs/startup-<时间>-<uuid>.log`（警告头 "WARNING: Raw diagnostics may contain configuration or credential values from plugin errors. Review before sharing." 逐字）后退出码 1（PRD 18.1、dsh startup-diagnostics.ts:34-67 + bin.ts:43-47）。
- realm 声明位 MUST：装载文档槽位支持 isolate 声明（dougong D15 isolate_realm；每会话 realm 挂载落码属后续切片，18a 只钉声明面，被 loader 接受）。
- 模型选择 rail MUST（2026-11-04 18b Q1A 裁决）：宿主装配时读组合后 llm 行 config 与 fr-schema 声明默认（provider 默认 `'deepseek-official'`，dsh base/cordis.patch.yml:90-94 判例同值）烘 `{provider, model, reasoning-effort?}` 进入口 facts 文档（与 app argv 同轨，决策 47 5A 延伸）；runner 从中继快照读取；dsh agentDefaultModel 服务缝不在 PRD 4.4 逐字清单（非 wire 偏差，内部结构落位）；saveSelection（写回）M1 无 UI 写路径=结构性缺席备案。
- runner 生命周期消费 MUST（2026-11-04 18b Q2A 裁决）：fr-headless descriptor inject += `["agent","session-persistence","session-query"]`——dsh runner inject `[agentDefaultModel, agents, sessions]` 的 fr 同构（index.ts 注入面）；服务双检文案逐字（sessionPersistence/sessionQuery 缺失两臂，index.ts:257-265）；create/resume/live 探测消费 fr-agent-loop 生命周期臂。
- 失败跟踪 MUST（2026-11-04 18b Q4B 裁决；2026-11-05 措辞勘误经用户裁决 A：泵轨无 session/turn/end 事件，dsh index.ts:369 原文即 session/event 按 type 过滤）：runner listen `agent/error` + `session/event`（按 event.type=='turn/end' 过滤）——`agent/error` 记 `{error, atOffset}`（同会话过滤），其后同会话 turn/end（seq ≥ atOffset）清零；收敛判定到达仍持未愈失败 → run 以该失败结束（stderr `fr:` 前缀错误行；--json 下 error 事件且无 final；exit 1）——dsh index.ts:365-372,391 1:1。
- 退出轨 MUST（2026-11-04 18b 裁决）：runner 完成经 `headless/exit` `{code}` 内核事件请求退出（插件→宿主唯一推送面=emit，执法事实：宿主不能 provide、require/dispatch 均插件向）；宿主 fr crate 内嵌 in-process 监听插件听该事件并触发有界关停携带该退出码；SIGINT 130/SIGTERM 0 覆盖语义不变。
- E2E 驱动 MUST（2026-11-04 18b Q3A 裁决）：集成测试以本地脚本化 SSE HTTP 服务器（测试内 TcpListener）+ profile patch 覆盖 fr-llm-deepseek base-url + DEEPSEEK_API_KEY 驱动全链真实生成，零 fixture crate。
- `--json` schema 工件 MUST（2026-11-04 18b Q5A 裁决，M1 验收 2）：fr-schema 增 json-stream schema 模块（八帧 schema 文档常量+校验器）；fr-headless 测试以真实事件流校验（dsh 无 schema 工件=fr 自有增补面）。
- 绑定与测试形态（2026-11-04 18b Q6 批准）：TTY 判定=注入判定单测（dsh startup.spec.ts:135 mock 判例同形）+进程级管道用例；场景 9=attempt 序列夹具投影级绑定（流错误重试 e2e 须 fr-llm-retry=18c，跨切片备案）；场景 21 四拒=夹具会话冷读+live 探测臂单测复合（fr 恒不发射 agent-preset/selected，preset 推进投影恒=header 值，损坏记录臂夹具构造）；场景 23=final 来源 e2e+提前唤醒抑制判定单测复合；help 文本=dsh cli-help.md 脚本抽取+品牌替换（fr-system-prompt extract 判例同轨，FR_DSH_SOURCE 缺席按设计 fail）。

## Boundaries

### Allowed Changes
- Cargo.toml
- crates/fr/**
- crates/fr-headless/**
- crates/fr-schema/**（2026-11-04 追加裁决：首次真装配实证 registry 将 plan-mode `section` 钉为 fr-tools-builtin 顶层必填，而 crate 侧该键烘焙不存在（plugin_shell.rs prompt_sections 静态表）——移除该幽灵必填并同片内调整钉测）
- crates/fr-agent-loop/** + crates/fr-tools-builtin/**（2026-11-04 追加裁决：首次真装配实证三 crate 描述符声明环 skills←agent←shell←tools-builtin，内核激活门控下组合结构性不可 boot；手术 = agent-loop descriptor inject 去 skills（dsh 软读边）、tools-builtin setup 增 grant(agent→skills) 惰性授权（时序：agent 剩余 inject 无环先激活→shell 解 park→tools-builtin 激活时 agent 已提交、grant 成立））
- crates/fr-session/**（2026-11-04 追加裁决：首次真装配实证工件壳未装配 session-persistence 服务带（dispatch 仅 declare/describe/projections，create/append/drain/read 等方法带从未接线、ShellState 无 SessionService——单测全部绕过工件直驱 service 面）；修复 = 工件 setup 装配 SessionService（烘焙根+core 词表+毫秒时钟+stderr 告警钩）并在 dispatch 路由 2-11 与 1002/1003 方法带，同片内补接线钉测）
- crates/fr-system-prompt/**（2026-11-05 18b d 首次真实 turn 实证裁决 A：装配臂 require builtin-tools.905（fr-tools-builtin 注册的 plan-policy textProvider endpoint）被内核能力门拒绝 undeclared_dependency。用户批准 A 后实证 inject += builtin-tools 关断声明环 system-prompt→builtin-tools→approval→system-prompt（boot fatal approval-policy Pending）——机制改按本仓既有 D31 判例（fr-approval 对自身 section 的 grant 同形、tools-builtin agent→skills 环断同形）：fr-tools-builtin setup 增 grant(system-prompt→builtin-tools)，fr-system-prompt descriptor 不动（inject=[]），该 crate 最终零改动、此边界行保留为实证记录）
- crates/fr-llm/** + crates/fr-llm-deepseek/**（2026-11-04 18b Q7A 裁决：runtime-registry 通道侧注册协议随 18b 落地——fr-llm REGISTER_ROUTE 臂+channel-proxy source+fr-llm-deepseek 生成面服务；真实生成 e2e 前置）

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 裸连字符从 stdin 读整段任务
  Test: stdin_task_flows_when_the_argument_is_the_bare_hyphen
  假设 TASK 为裸 - 且 stdin 提供整段任务文本
  当 运行 fr headless 加该参数时
  那么 整段文本作为任务且不再等待更多输入

Scenario: 给了 TASK 不读 stdin
  Test: given_task_skips_the_stdin_read
  假设 命令行已给定 TASK
  当 运行时
  那么 stdin 被忽略且不阻塞

Scenario: 无 TASK 且 stdin 为 TTY 报 usage error
  Test: tty_usage_error_and_its_message
  假设 终端交互运行且未给 TASK
  当 执行 fr headless 时
  那么 输出 usage error 且 exit 非 0

Scenario: 完整任务走通并产出 v4 会话
  Test: json_run_completes_with_the_first_and_last_frames_pinned
  假设 有效 provider 配置
  当 执行 fr headless 加任务时
  那么 最终答案到 stdout、产生 v4 会话文件且 exit 0

Scenario: 运行错误走 stderr 且 exit 1
  Test: provider_error_fails_with_the_fr_prefixed_line_and_exit_one
  假设 provider 配置错误
  当 运行失败时
  那么 stderr 输出 fr 前缀错误行、exit 1 且 stdout 无答案文本

Scenario: reasoning 流到 stderr
  Test: default_mode_prints_the_answer_and_streams_reasoning_to_stderr
  假设 模型输出含 reasoning delta
  当 默认模式运行时
  那么 reasoning 实时写入 stderr 且带 fr 前缀标题，stdout 不混入

Scenario: --json 首末事件定型
  Test: json_run_completes_with_the_first_and_last_frames_pinned
  假设 --json 模式运行成功
  当 解析 stdout 事件流时
  那么 首事件 type 为 session，末事件 type 为 final 且答案无损

Scenario: 中间事件类型齐备
  Test: projection_opens_with_the_session_frame_and_folds_the_run
  假设 任务触发工具调用
  当 解析 --json 流时
  那么 status、text、thinking、tool_call、tool_result 事件按序出现

Scenario: 丢弃的 attempt 不投影
  Test: committed_blocks_project_as_text_and_thinking
  假设 流错误后重试成功
  当 解析 --json 流时
  那么 text 与 thinking 仅来自已提交 assistant message，重试尝试文本不出现

Scenario: 值截断封顶
  Test: bound_line_truncates_strings_and_flags
  假设 某 tool_result 值超过 8 KiB
  当 输出该事件时
  那么 值被截断且标 truncated 为 true，单行不超过 32 KiB

Scenario: 运行错误输出 error 无 final
  Test: uncured_json_run_emits_the_error_event_without_final
  假设 --json 模式运行失败
  当 输出事件流时
  那么 含 error 事件且无 final 事件

Scenario: 需审批工具收到 unavailable
  Test: set_approval_transport_failure_fails_closed_unavailable
  假设 模型调用需审批工具且无 answerer
  当 工具进入审批时
  那么 模型收到 unavailable 拒绝结果

Scenario: danger-full-access 直接放行
  Test: the_baked_default_follows_the_bundle_expression
  假设 FR_PERMISSION_MODE 为 danger-full-access
  当 工具执行时
  那么 策略为 never 且工具直接执行

Scenario: 续用已持久化会话
  Test: resume_path_adopts_and_rechecks
  假设 存在一个已持久化的非 fork 非 子代理会话
  当 以 --session-id 指定该 id 运行时
  那么 接续其历史继续且新事件追加到原会话

Scenario: 不存在的 session id 失败
  Test: missing_session_id_fails_with_the_omission_refusal
  假设 --session-id 指向不存在的会话
  当 启动时
  那么 直接失败且不创建新会话

Scenario: 默认生成新会话 id
  Test: json_run_completes_with_the_first_and_last_frames_pinned
  假设 未指定 --session-id
  当 启动时
  那么 创建形如 session 加 uuid 的新会话

Scenario: headless 装配无交互运行时
  Test: headless_assembly_boots_real_dylibs
  假设 headless profile 装配完成
  当 检查装配产物时
  那么 无 Host/HTTP/Web 运行时插件、无交互式凭证插件、无 UI answerer

Scenario: --help 输出用法
  Test: help_prints_the_usage_and_exits_zero
  当 运行 fr headless --help 时
  那么 输出用法说明且 exit 0

Scenario: 语法错误走 JSON 契约
  Test: json_usage_error_also_emits_the_error_event
  假设 --json 模式传入非法参数组合
  当 解析失败时
  那么 stdout 输出 error 事件行且 exit 1

Scenario: key 级截断与降级梯
  Test: long_keys_truncate_and_collide_last_wins
  假设 事件含超长 key 与超大值
  当 boundJsonLine 处理时
  那么 key 与字符串值均封顶 8KiB，行超限依次降级为标量加 truncated 再到仅 type

Scenario: 采纳四拒与 live 拒绝
  Test: preset_mismatch_is_named_before_the_live_refusal
  假设 --session-id 分别指向 preset 不匹配、子代理、cwd 不一致或已被 live Agent 占用的会话
  当 采纳时
  那么 一律拒绝且给出对应错误

Scenario: trim 空任务参数报错
  Test: blank_session_id_and_blank_task_fail_verbatim
  假设 任务参数为连续空白非空串
  当 启动时
  那么 报错且 exit 非 0；--session-id 空白串同理判空失败但原串保留于错误上下文

Scenario: final 来源与提前唤醒抑制
  Test: summarize_takes_the_last_nonempty_text_and_the_final_reason
  假设 会话日志含多段 assistant 文本且某子代理在收尾时提前唤醒主循环
  当 汇总输出时
  那么 final 取 durable log 最后一段非空 assistant text，唤醒以 seq 未变且无存活子判定抑制、继续等待

Scenario: usage 累加与省略
  Test: usage_accumulates_and_omits_partial_steps
  假设 同一 step 跨两次 attempt 且其中一次无 usage 样本
  当 投影事件时
  那么 有效样本跨 attempt 累加；任一 attempt 无样本则整 step 省略 usage

Scenario: tool_result 仅 append 投影
  Test: replaced_results_never_project
  假设 某工具结果曾被压缩替换（surfaceOp 非 append）
  当 投影 --json 流时
  那么 原始结果不出现，仅 append 形态的 tool_result 投影

Scenario: agent/error 无终结事件不愈即败
  Test: uncured_agent_error_fails_the_run
  假设 agent/error 记录失败于某 seq 且其后无 seq ≥ 该值的 turn/end
  当 收敛判定到达时
  那么 run 以该失败结束：stderr fr 前缀错误行、--json 下 error 事件且无 final、exit 1

Scenario: headless 组合经真实 dylib 装载走通
  Test: headless_assembly_boots_real_dylibs
  假设 各第一方插件 crate 的 cdylib 工件已构建
  当 宿主壳以 fr-config 编译 headless 组合并交 dougong loader 装载时
  那么 全部第一方槽以 dylib 源装载成功、各 provide 服务可 require、槽位 isolate 声明形态被 loader 接受

Scenario: 装载清单无 builtin 源
  Test: assembly_manifest_has_no_builtin_sources
  当 检查 headless 组合编译产物的每个槽位时
  那么 不存在 kind 为 builtin 的源（决策 16 红线）

Scenario: 入口事实经文档行注入
  Test: entry_facts_injected_through_document_rows
  假设 宿主壳解析到根 argv
  当 组装装载文档时
  那么 app argv 快照落在入口插件槽位 config 中，且经宿主在 boot 窗中继至入口插件自持的 headless-startup 服务后可读回

Scenario: 装配失败启动诊断落盘
  Test: startup_failure_writes_diagnostic_log
  假设 装配在 boot 期遭遇致命错误（如坏配置）
  当 启动诊断面处置该错误时
  那么 stderr 输出单行摘要、完整报告落盘 $FR_HOME/logs/startup-<时间>-<uuid>.log 且含逐字警告头，随后退出码 1

Scenario: 信号映射退出码
  Test: signals_map_to_dsh_exit_codes
  当 宿主壳收到 SIGINT 或 SIGTERM 时
  那么 SIGINT 映射退出码 130、SIGTERM 映射退出码 0，且先广播 shutdown 再有界 join 插件自有线程

Scenario: 事件泵送达 listened 事件
  Test: event_pump_delivers_listened_events
  假设 装配完成且订阅插件 descriptor 声明 listen 表
  当 发布插件 emit 事件时
  那么 事件按 listen 表送达对应订阅插件 on_event 且载荷逐字无损

## Questions

- Source trace: doc/prd.md#18.1: headless 字段级规格（CLI、输出、--json、审批、多轮）, confidence: high（PRD 18.1 字段级）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
