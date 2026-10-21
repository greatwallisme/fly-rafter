# dsh 源码技术关键点笔记（合约回灌依据）

| 项 | 值 |
|---|---|
| 用途 | 4 个源码探查代理的原始报告，作为合约条款/场景回灌与 PRD 勘误的 file:line 依据 |
| 状态 | 汇总中（4 份，本文随报告返回追加） |

## 报告 1：headless / SDK / ACP / base 装配（2026-10-21）

### PRD 与源码不符（需用户裁决）

1. SDK 未知方法错误码：PRD 18.4 称 -32601；dsh 实现中 SDK 侧恒有 handler，未知方法经 handleRequest default 抛普通 Error 转 -32603（-32601 仅 transport 未装 handler 时出现）[packages/sdk/protocol/src/transport.ts:226-237 + packages/sdk/server/src/server.ts:374-375]
2. bash timeout 60000 归行：dsh 挂在 bash-sandbox 行 [BASE/cordis.patch.yml:249-253]，非"bash 工具"行；行为值一致，仅装配归行差异

### headless（PRD 未记载）

- `-` 必须是唯一任务参数；args 非空但 trim 空报错 [HL/src/startup.ts:96-106]
- --json 模式下 Commander 语法错误也发 JSON error 事件（覆写 program.error，剥前缀写 stdout）再 exit 1 [HL/src/startup.ts:83-94]
- --session-id trim 后判空但原串交付（空白属身份）[startup.ts:110-113]
- assertAdoptable 四拒：preset 不匹配（含 corrupt 事件）、subagent/fork、无 cwd、cwd 与当前不一致 [HL/src/index.ts:212-231]
- live Agent 占用直接拒绝；resume 双重复查（query 快照 + 写租约后）；sessionPersistence+sessionQuery 服务前置 [index.ts:257-292]
- followup 后循环等 whenIdle + 托管子孙（子代理收尾提前唤醒则以 seq 未变且无子判定）[index.ts:375-386]
- 汇总 summarize 取 durable log 最后一段非空 assistant text + turn/end reason [index.ts:73-99,390-398]
- --json：首事件 {type:'session',sessionId,cwd} 订阅前写出 [json-stream.ts:318,322-326]；final 不经截断（无损豁免）
- 截断精确数值：字符串与 key 均 8KiB（8192 字节）、整行 32767（32KiB 扣换行）、深度 64；二级降级梯（整行超限→标量字段+truncated→仅 {type,truncated}）[json-stream.ts:16-22,59-66,132-149]
- usage 跨 attempt 累加，任一 attempt 无样本则整 step 省略 usage [json-stream.ts:266-286]
- tool_result 仅 surfaceOp==='append' 时投影 [json-stream.ts:300-303]

### SDK（PRD 未记载）

- 请求 id 形如 req_<uuid>；params 非对象坍缩为 {} [transport.ts:121-156,271-274]
- subagent.finished 仅 info.local 子女（非 local 不过）[server.ts:103-147]
- status 映射：completed→ok；max-tokens 仅 options.maxTokensAsSuccess 才 ok（默认 false）[server.ts:73-76; server/src/index.ts:36]
- initialize：reasoningEffort 非空串/maxTokens 正安全整数校验；无适配器时仅 provider=='deepseek-official' 允许惰性挂 LlmDeepSeek；resolveCallConfig 预检路由 [server.ts:155-189]
- wait：未知 id 报错从不创建；循环 whenIdle→waitForChildren→seq 未变才退；退出有 failure 则抛 [server.ts:240-257]
- shutdown 单飞：等在途创建→清 map→逆序 pop disposers→drainDescendants→并行 dispose→失败聚合 [server.ts:298-334]
- onRequest 前对 initialize 先等 loader 就绪；shutdown 响应后 setImmediate→flush→dispose→exit 0 [server/src/index.ts:65-100]

### ACP（PRD 未记载）

- image 能力为条件通告（attachment 支持栅格式且模型声明 image 输入）；audio/embeddedContext 恒 false [ACP/src/index.ts:180-194; content.ts:89-100]
- resume：活跃/activating 拒绝；persistence.stat 取 header 拒 subagent/fork；cwd realpath 物理等同比较；恢复优先"最后一次记录的路由" [index.ts:243-294; session.ts:78-94]
- list：过滤活跃/子代理/fork/cwd 缺失非绝对；createdAt 降序同刻按 id 字节序；keyset 游标 base64url 强校验 canonical [index.ts:296-335,479-506]
- prompt 准入：单 in-flight 抛 invalidParams；准入期两次 agent 存活校验；图片未通告即 invalid、严格 canonical base64 [session.ts:246-335; content.ts:46-58,123-137]
- 模型 pin：准入时 snapshot→pendingSelections（按 messageId）→claimed 时 pinTurn（turnSelection 优先于 selected）→turn/end releaseTurn [session.ts:279-410; model-control.ts:75-85]
- 每会话串行更新 = outputTail promise 链；交付失败仅记 outputError 不阻断 [session.ts:349-398]
- settlement 循环 race(whenIdle,cancelled)→waitForChildren→outputTail；cancelled→resolve('cancelled') [session.ts:512-565]
- request_permission 仅 owned 且有 callId 拦截；先 drainUpdates；allow-once/reject-once 两选项 [index.ts:159-177]
- config id 仅 'model'/'reasoning_effort'，未知 id 报错；configOptions 经 tail 串行化 [model-control.ts:130-141]

### base 装配树与未记载默认值

- base cordis.patch.yml 单 insert 列表共 90 行 id（tool-plugin-manager、plugin-manager、timer、hmr、llm、session、agent、jobs、approval、permission、tool-bash、tool-fs、compaction-basic、subagent*、ptc-runtime、tools、system-prompt、agent-loop、fs-sandbox、llm-deepseek 等，完整名单见代理报告）
- PRD 6.4 未记载默认值：agent-instructions maxBytes 65536（:306）；session-projection-cache 200 事件/5000ms（:199-200）；web searchProvider=deepseek-official、fetchProvider=http（:470-471）；tool-web fetch:true、searchTimeoutMs 60000（:489-490）；repeat argumentsPreviewChars 500（:451）；标题 maxOutputTokens 4096（:70-77）；tool-result-pruner 8192/4096/1024（:428-430）；spill maxInlineTokens 12500（:417）；permission presets 三档表（:264-276）；plan-mode 提示段（:332-346）；tools 默认 native（:499-500）；storage-domain backend json（:190）

## 报告 2：工具系统 / PTC 运行时（2026-10-21）

### PRD 与源码不符

1. tool/call 追加时机：PRD 9.2 画在管线末端；源码在 prepare（pre-execute）之前 append [core/agent-loop/src/tool-calls.ts:168]，tool/result 由 contiguous 游标按 model 序在 finalize 后 append 并 cite callSeq [tool-calls.ts:146-161]
2. 绑定名规则：PRD "拒绝保留字/console/dunder/__proto__"；源码 dunder 仅拒 error member（RESERVED_ERROR_MEMBERS，不匹配 DUNDER 于 member）；binding global 不拒 dunder；__proto__ 不拒绝——按普通 own key 注入（宿主 null-prototype+defineProperty [tools/src/ptc.ts:686-694]，child 同 [ptc-runtime-node/src/bootstrap.ts:313-358]）[bindings.ts:7-53; ptc-runtime/src/index.ts:42-66]
3. PTC wire 帧格式：PRD "长度前缀/换行"；源码仅 4 字节 BE u32 长度前缀 + JSON 体，无换行帧 [ptc-runtime-node/src/channel.ts:59-65]
4. 程序文件系统访问：PRD "程序不读文件系统"；源码执行指令明示 fs 访问按沙箱策略执行（非禁止）[ptc-runtime-node/src/index.ts:48]

### PRD 未记载（关键技术点）

- 管线细节：ptc 坍缩调用（model-direct 非 run_code）在策略管线前终结（UNKNOWN_TOOL+路由文本，绕过 pre-execute/guards）[tools/src/index.ts:1461-1482]；guards 仅 allow 时求值、global 先 agent 链 farthest-first、单调无 allow [index.ts:1153-1162,724-732]；guard 拒绝走 post-result 仍过 post-execute [index.ts:1501-1547]；post-execute block→isError+feedback、value/content 双替换抛 TypeError [index.ts:1789-1828]；finalize 冻结 exec 再 emit tools/result [index.ts:1677-1722]
- 调度：isConcurrencySafe 仅严格 true 才 parallel（fail-closed exclusive）[index.ts:1311-1320]；exclusive 组=仅首调用屏障、其余 parallel 排队 [agent-loop/src/tool-calls.ts:89-90]；parallel 池上限=agentLoop.config.maxParallelToolCalls 默认 10 [constants.ts:6]；启动前 lazy 重分类 [tool-calls.ts:200-213]；concludeTurn 仅成功结果可携带 [index.ts:593]；abort 合成补偿文本 ABORTED_BEFORE_DISPATCH [tool-calls.ts:238-260]
- schema 推断：const>enum>宽类型、integer→number、array items 缺省 JsonValue[]、oneOf 并集、深度 16 上限 [schema.ts:150-166]；object additionalProperties 必须显式 boolean [schema.ts:367-369]；JSON Schema 强制子集白名单 [json-schema.ts:6-10]
- presentation：ToolCallKind 8 值；call 视图三族/result 六族；search 用 shape 判别、web 用 kind；软校验失败降级 generic 卡片 [presentation.ts:15,46,140,215-239,281-307]
- PTC 硬数值：默认 timeoutMs 120s、maxTimeoutMs 600s、maxOutputBytes 64MiB、maxOldGenerationSizeMb 512、maxMessageBytes 128MiB、maxPendingCalls 128、graceMs 3s [ptc-runtime-node/src/config.ts:31-44]
- 8 类失败精确触发条件（exception=strip 抛/程序抛；timeout=wallTimer；abort=signal 非超时；worker-exit=先行退出/输出流失败/清理失败；invalid-output=非 lossless JSON；output-limit=LogBuffer 预算/admit 溢出/ledger 超限重写；protocol=非法帧/坏 log/坏 call id（须严格递增）/未声明绑定/pending 双上限；sandbox-unavailable=SandboxUnavailableError/runner spawn 规则）[ptc-runtime-node/src/index.ts:200-339; bootstrap.ts:47-96,176-182; output-ledger.ts:24-65]
- strip-only：程序包成 async function __dsh_program__ 后交 Node stripTypeScriptTypes [index.ts:30,200-203]；AsyncFunction('use strict') 求值 [bootstrap.ts:404-411]
- 声明生成：工具按 name 字典序、键非裸标识符 JSON 引号、JSDoc 折叠转义 */、additionalProperties!==false 追加 Record、敌意 schema 降级 unknown [tools/src/ts-types.ts:18-23,32-38,169-171,240-247]；PTC_ONLY order=800 [system-prompt/src/index.ts:136]
- 子调度：dispatch-start 于真正获得槽位时追加 [ptc.ts:613-620]；dispatch-log waterfall 听众抛错 contained 回退 [index.ts:1331-1341]；背压 logWork>maxParallel [ptc.ts:662]；排队未启动者 abandon 不记日志 [ptc.ts:524-532]
- 审批升级：permissions/justification 成对非空；同模式免审、须严格更宽否则抛、ESCALATION_TARGETS=[workspace-write,danger-full-access] [sandbox/src/escalation.ts:41-63,165-217]
- persistent：每 state owner 一进程、单并发、换策略须先 release [ptc-runtime-node/src/persistent.ts:69,125,334-356]

## 报告 3：LLM 层 / 系统提示词 / 审批（2026-10-21）

### PRD 与源码不符 / 未记载

1. CONTEXT_ORDERS 权威行号：PRD 写 ：149，源码在 system-prompt/src/index.ts:170（数值一致）
2. dsh 无独立 openai/anthropic provider 目录：llm-pi-ai 单适配器覆盖 openai-completions/openai-responses/anthropic-messages；fr 三插件拆分为自有结构（结构性自由度）
3. AGENTS.md 注入形态：不是 system section——渲染为 user 消息包裹 system-reminder 帧（"Instructions from: <path>"）[context/agent-instructions/src/render.ts:315-321]；变更/删除以 Updated/removed 语义再注入 [render.ts:255-272]；fs 工具触碰嵌套指令文件触发再投影 [index.ts:4-5]
4. LlmAdapter 方法清单 PRD 漏 imageRequestTarget [llm/src/index.ts:248-250]

### PRD 未记载（关键技术点）

- StreamChunk 字段全集与 ReplayEnvelope{response,blocks}；丢块时 envelope 同位置过滤、长度不等整体丢弃 [llm/src/types.ts:470-482,448-467; assembler.ts:141-149]
- 装配结算：已 close 索引上的 delta 忽略、重复 close 忽略、delta-only 协议 ensure 兜底建块、tool-call 缺 id/name 兜底、无 finish 默认 {kind:'stop'} [assembler.ts:49-106,115-116,187-189]
- max-tokens：丢弃全部 tool-call 块且 envelope 同步过滤 [assembler.ts:135-150]；中断：只保留非空白 text/reasoning，tool-call 一律省略 [assembler.ts:169-179]
- attempt 结算：aborted 且中断内容非空→assistant/message{interrupted:true}；否则 assistant/attempt；error/aborted→attempt+request-error waterfall 可 retry [agent-loop/src/agent.ts:464-545]
- 持久化打包四 record（text/reasoning/tool-call-chunks/chunk）、dt 差分时间、id/name 不全的 tool-call-delta 落 raw、block-start/end/usage/finish 恒 raw [llm/src/assistant-stream.ts:107-190]
- prepare_call 代际绑定：PreparedAdapterCall 冻结元数据、stream 一次性（二次派发 INVALID_PREPARED_CALL）、callConfigEquals 防跨代 capability [llm/src/index.ts:956-1020]
- retry policy：mode normal（maxRetries 5、retryableCodes 五类）|always；backoff 500ms/10s/jitter 0.1 对称抖动；执行方是 llm-retry 插件挂 agent 失败步扩展点 [llm-retry? retry-policy.ts:222-262,211-215]
- deepseek：messagesApiRoot 强制补 /v1；SSE 经 eventsource-parser，帧 event 与 body.type 不一致→MALFORMED_RESPONSE；message_stop 校验 stop_reason+全块 closed+tool arguments 合法 JSON；stop_reason 映射表；usage message_start 起底 delta 覆盖 [llm-deepseek/src/*]
- pi-ai：永不中途 throw，错误事件→usage+error/aborted finish；toolcall_end 把已解析参数 stringify 回原始串（与 deepseek 逐段拼接同契约）[llm-pi-ai/src/stream.ts:216-227,202-204]
- toolUpdate 能力差异：anthropic→in-history、openai-completions→addition-only、responses 系无 [llm-pi-ai/src/models.ts:80-87]
- SECTION_ORDERS 全表 32 项（HARNESS_IDENTITY -1000 … DEPLOYMENT_PERSONA_SUFFIX 10200）；同 order 按 name code-unit 序、重名抛错、scoped 遮蔽 global [system-prompt/src/index.ts:131-165,248-250,407-415,631-632]
- toolOrder：保留标记 <unlisted-tools> 恰好一次、未列工具字典序插入 rest 位 [index.ts:202-239]；{{var}} 变量名 ^[a-z][a-z0-9_]*$、畸形/未注册抛错指明 section、interpolate:false 保留字面 [index.ts:196,290-377]
- complete section 至多一个、waterfall 后恢复唯一 prompt section；refreshContext 只重跑 context [index.ts:653-656,582-602]
- AGENTS.md：候选 AGENTS.md/CLAUDE.md + local 变体；projectRoot 从 cwd 向上找 .git marker；maxSourceBytes 1MiB 超限跳过、maxBytes 二分截断到恰好适配、同组去重 [agent-instructions/src/config.ts,files.ts,render.ts]
- 审批补充：approval/policy log-only 不进 transcript；setPolicy 同值 no-op、切换注入文案 "The approval policy changed from "x" to "y" (changed by the user)."；hasOpenTurn 从日志尾倒扫；'never' 在 dispatch 前由服务自身判 rejected（防 listener 绕过）；waterfall promise.then 先入链；abort 竞速先赢 cancelled；ApprovalRequestId fresh uuid；effectivePolicy=override??config??'ask' [user-approval/src/index.ts 各处]

## 报告 4：会话系统 / agent 循环（2026-10-21；报告尾部生成退化，正文可用）

### PRD 与源码不符 / 措辞不准

1. SessionHeader：PRD 称含 seedLength——源码无此字段且显式拒绝；实际 isSeeded + 必填 delegationDepth，继承切点写在最后一个 tagged session/end-seed [session/src/index.ts:98-100; session-persistence-jsonl/src/format.ts:113-118]
2. 空渲染语义：非"清空全部 system 节点"——清空活跃尾部节点且 head 被 replace 为空文本（head 亦占节点）[agent-loop/src/runtime-context.ts:104-110]
3. 迁移链：dsh 实现完整 v0→v4 相邻迁移链；fr 按 PRD 裁剪 v4-only，但验收对照不得以 dsh 迁移行为为准 [session-format-catalog/src/generated.ts:68-97]
4. resume 拒绝子代理/fork：core/session 无强制，门禁在 host 层（headless assertAdoptable）——fr 须自行实现该门禁

### PRD 未记载（关键技术点）

- 编码/发布：头部必需键 type/version/id/createdAt/isSeeded/delegationDepth，origin 只允许 'subagent'；种子信封六键、seq 从 0 连续；首发=临时文件(wx,0o600,fsync)→link()+unlink（禁 rename）→逐级目录 fsync；append 失败 truncate 回滚；zstd 撕裂尾完整记录恢复；迁移发布 stage→verify→barrier→link，EEXIST 逐字节比对 [session-persistence-jsonl/src/*]
- surface：仅 5 类事件入表面（system/developer/user/assistant message、tool/result）；replace startSeq/endSeq=表面节点双闭区间且必须早于当前 seq；sourceEventSeqs 硬约束（非空/无重复/全部更小/必须覆盖全部被遮蔽节点/assistant 禁带）；tool/result 替换只许改 content（深相等比较置 null）；node-0 只许 system/message 单节点重写；replaceGeneration/contentGeneration 双计数；seq-ranges 折叠编码（连续 run≥3 → [start,end]）[session/src/surface.ts, seq-ranges.ts]
- deriveMessages：contentGeneration 变化整表重建、尾增长增量投影；空 assistant（承载 max-tokens usage）投影 null；deepFreeze(header)→逐消息 deepFreeze→freeze 数组 [session/src/index.ts:984-1005; agent.ts:686-694]
- request/header：reason 算法（首条 initial/resume，其后仅 headerEquals 变化才 change、仅序列边界才 series）；canonicalHeader 空 tools 省略、tool schema JSON.stringify 顺序敏感比较；工具增删 developer/message 仅 baseline 存在且名字集合有差，headerSeq 仅 additions>0 携带 [agent.ts:635-665; request-header.ts:21-52]
- runtime-context：以 user/message(source kind 'runtime-context') 进表面；清空文案固定 'Current runtime context: none. Earlier runtime-context snapshots no longer apply.'；恰好 admitted 一次 [runtime-context.ts:14-16,96-119; agent.ts:425-440]
- inbox claim：一次性取走全部 next-step、'next-turn' 至多 1 条；splice 规范化与 message id 唯一；每次变更加 agent/inbox/spliced [inbox.ts:109-114,198-243]
- turn/step 状态机：相位 idle/maintenance/running；reject→turnEnds={blocked}；step0 空→completed（占首边界不花模型调用）；turnEnds 后空→静默 break；max-tokens 粘滞不得被 completed 降级；turn-stopping 仅 turnEnds 已设且 nextStep 空才派发；每 turn 换新 AbortController 使旧闩失效 [agent.ts:154-161,298-397]
- 重试结算：error/aborted→attempt→request-error waterfall（默认 undefined）→非 retry 抛 LlmError；user/message 仅 firstAttempt admitted [agent.ts:504-527,426-441]
- abort 两级补偿：调度器跳过=ABORTED_BEFORE_DISPATCH（'Error: tool call aborted before dispatch'，AbortError）；步级/崩溃恢复=TOOL_NOT_STARTED / TOOL_OUTCOME_UNKNOWN（repair.ts 精确文案 CLOSER_TEXT）；seq 从 last.seq+1 连续、time 复用最后真实事件（确定性）[tool-calls.ts:250-260; repair.ts:15-197]
- NO_ADAPTER 容忍（config=proposal 直发）；缺 provider/model 直接报错 [agent.ts:573-610]
- ptc 子调用 id 格式 <parent>:ptc:<n>；每个已启动子调用恰好结算一次 [tools/src/types.ts:42-64]

### 补遗（2026-10-21，生成数据逐条核对发现的笔记缺口，源自报告 2/3 原文）

- PTC 默认配置精确值 [ptc-runtime-node/src/config.ts:31-44]：executionMode 'per-call'；timeoutMs=120_000；maxTimeoutMs=600_000；maxOutputBytes=67_108_864(64MiB)；maxOldGenerationSizeMb=512；maxMessageBytes=134_217_728(128MiB)；maxPendingCalls=128；graceMs=3_000
- SECTION_ORDERS 全表 32 项精确值 [system-prompt/src/index.ts:131-165]：HARNESS_IDENTITY -1000、DEPLOYMENT_PERSONA_PREFIX 0、PLAN_POLICY 500、TEAM_POLICY 600、PTC_ONLY 800、FILE_REFERENCE 900、TOOL_BASH 1000、TOOL_PWSH 1010、TOOL_READ 1100、TOOL_WRITE 1200、TOOL_EDIT 1300、TOOL_GLOB 1400、TOOL_GREP 1500、TOOL_JOBS 1600、TOOL_PTY 1700、TOOL_WEB_SEARCH 2000、TOOL_WEB_FETCH 2100、TOOL_LSP 2200、TOOL_SESSION_QUERY 2300、TOOL_GOAL 2400、TOOL_WORKFLOW 2600、TOOL_RALPH 2700、TOOL_SUBAGENT 2800、TOOL_REPORT 2900、TOOL_COMPUTER_USE 3000、MCP_SERVERS 3100、TOOLS_SDK 5000、DELIVERABLE_FILE_REFERENCES 9000、STRUCTURED_OUTPUT 9900、HARNESS_SOURCE 10000、WEB_SURFACE 10100、DEPLOYMENT_PERSONA_SUFFIX 10200
- CONTEXT_ORDERS 全表 7 项 [index.ts:170-179]：WORKING_DIRECTORY 100、SANDBOX_POLICY 110、APPROVAL_POLICY 115、SUBAGENT_DELEGATION 120、TEAM_IDENTITY 125、HARNESS_SOURCE 200、WEB_SURFACE 210

## 报告 5：斜杠命令框架 / 子代理系统（2026-10-21）

### A. 斜杠命令

- 注册：ctx.commands 插件名 'commands' [interaction/commands/src/index.ts:30]；内置宿主命令仅 /goal（command-goal，hint `[<objective>|clear|edit <objective>|pause|resume]`）、/compact（无参，`Usage: /compact (no arguments)`）、/feedback（recordInput:false，hint `<text>`）、/export（Web 版拒绝路径参数）、/plan（hint `[off|message]`，非 off 消息经 agent.steer 转发）、/permission（空参报当前 preset 与可用表）；/help 不存在（无注册）
- 解析 parseCommand [commands/src/index.ts:125-132]：`/^\/([a-z][a-z0-9_-]*)(?=$|[\t\n\r ])/u`；rawInput=精确余串含分隔空白永不归一；无引号/转义规则；非命令返回 undefined 且不记日志；不以 / 开头（或裸 /）走模型普通输入
- 注册校验：名称 `^[a-z][a-z0-9_-]*$`、描述/handler/hint 非空；同层重名报 `command "X" is already registered…`；scoped 定义遮蔽 global；list() 名称序；变更发 commands/change
- 生命周期：命令从不发给模型 [index.ts:76-77,333]；handler 前 append command/run、结算后 command/done（log-only，无 turn）；command/run 载荷 {commandId,name,args?,source:{kind:'user'}}（recordInput:false 省略 args）；command/done 载荷 {commandId,kind:'success'|'error',text?,sourceEventSeq?}；commandId=`cmd-<8位uuid片>-<单调序>` [index.ts:446-449]
- handler 抛错/中止 → command/done kind:'error' 后重抛 [index.ts:426-443]；带附件给无附件命令 → `/<name> does not accept attachments`；无 store → `…attachments are unavailable because no attachment store is composed`
- /goal 语义 [command-goal/src/index.ts:40-160]：空=显示（title/Status/Blocker/Objective/Rounds/Activation/Commands 块）；clear/pause/resume/edit；其余=create；错误文案：已设 goal（:125-131）、missing-goal（:108-112）、GoalError→"The goal command is not valid for the current state. Run /goal to view available commands."（:157-160）；附件在 create/edit 作 followup 用户消息 "Reference attachments for the goal objective."（:96-103）、非 create/edit 报错；clear 无 goal → 成功 "No goal to clear."
- /compact [command-compact/src/index.ts:26-107]：成功 `Compacted N history items (~M tokens).` + sourceEventSeq；无历史 `No compactable history yet.`；取消 "Compaction cancelled."；ManualCompactionError 码 busy/cancelled/changed/summary/commit/persistence 各有固定文案
- /feedback：空白 → "Feedback text is required. Usage: /feedback <text>"；成功 "Feedback recorded for session <id>\nAnonymous user: <id>."；append log-only feedback/record
- /plan [plan-mode/src/index.ts:229-272]：off → "Plan mode off."/"Leaving plan mode (applies from the next step)."/"Plan mode entry cancelled."/"Plan mode is already inactive."；on → "Plan mode on. Use /plan off to leave." 或 "Entering plan mode (applies from the next step).…"；非 off 消息与附件 agent.steer 为用户消息；off 带附件 → "Attachments cannot accompany /plan off."
- /permission [permission-presets/src/index.ts:255-273]：空参 → "current preset X (available: …)"；未知 → `unknown preset "X" (available: …)`；成功 → `preset X`

### B. 子代理

- 服务 ctx.subagents [subagent/src/index.ts:194]；默认 maxDepth 1、maxActiveSubagents 8 [index.ts:195-198]
- child id：local=randomUUID 会话 id [manager.ts:385]；external=provider 铸造全局唯一 [types.ts:288]；重名 → `subagent "X" already exists` 码 DUPLICATE_CHILD [manager.ts:634-638]
- subagent 工具即时返回 {kind:'activation',subagentId} 渲染 `started subagent <id>` [tool-subagent/src/index.ts:264-274,337]；父日志收 subagent/catalog 分块清单记录（childId/label/mode）[subagent/src/catalog.ts:139-170]；生命周期边=Cordis 事件 subagent/start / subagent/end（载荷 {runId,provider,id,local} / +stopReason,lastAssistantMessage?）[index.ts:142-166,lifecycle.ts:49-52,96-116]
- 结果送达：manager 清理后以 user 消息投递（父空闲 queue、运行中 steer）[manager.ts:1187-1206]；文案族 [continuation-messages.ts:106-167]：`Background subagent <id> finished and will do no further work unless you send it more.` / `…finished. It cannot receive follow-up messages.` / `…was stopped before it finished.` / `…ran out of room…` / `…declined the task.` / `…failed before it finished.` + `Its closing message:`（无则 `It left no closing message.`）+ 可选 `Structured result: <json>`；source {kind:'subagent-settled',form:'notice',summary,senderSessionId}；stop reasons completed/aborted/error/max-tokens/refusal [types.ts:233-247]；可续 provider 的子初始 prompt 追加返回指引（父 id + send_message 指令）[manager.ts:460-463]
- fork [subagent-fork-in-process/src/index.ts:46-79]：种子=父日志最后 turn/end 为止的均衡完成 turn 前缀（in-flight 排除，无完成 turn 则空）；创建时一次捕获成子自有持久 transcript；继承 preset 组合与委托策略；cwd 默认父生效目录；spawn 不共享任何对话上下文（inheritsParentContext:false，agentPreset 能力 true）；fork agentPreset:false、inheritsParentContext:true
- interrupt_agent [tool-subagent-control/src/index.ts:74-111]：同步输出 `interrupt requested for agent <id>` {accepted:true} 不等待；运行时发取消信号目标可继续到观察点；未领取 inbox、Activation、已发布后代保留；external 永久停止 [subagent/src/index.ts:344-361]；local → agent.cancel({kind:'user'|'parent'},{keepInbox:true}) [manager.ts:221-222]；目标不存在=accepted no-op [manager.ts:203-204]；UNAUTHORIZED：非活跃祖先/self-interrupt/他父/非活跃后代 [manager.ts:188-216]
- send_message [tool-subagent-control/src/index.ts:28-72]：运行中→下 step 准入（steer）、空闲→开新 turn（queue/followup）[manager.ts:615-628]；渲染 `message delivered to agent <id>` 返回 {messageId}（送达确认非答案）；child→parent 包裹 `Agent <id> sent a message: …` source {kind:'agent-message',form:'relay'} [manager.ts:556-563]；缺席 direct child 从持久层冷续 [manager.ts:1230-1256]；非驻留 child 发父拒绝 `…cannot send to parent…` UNAUTHORIZED；external child → NOT_CONTINUABLE
- list_agents [list-agents.ts:50-149]：scope children（默认，直接子代自持久 catalog 不加载 Agent）| descendants（递归前序，行含 parent 与 depth）；one-shot/external 子代不列出；诊断行 {kind:'diagnostic',reason:'corrupt'|'unsupported'|'unavailable'}；status running/inactive 自活跃注册表；空渲染 `(no subagents)`；行格式 `<id> [<status>] parent=<p> depth=<d> — <label>`
- 容量与深度：per-root ActivationPool 经不间断可续父链全后代共享 [manager.ts:743,840-848]；超限 → `subagent limit reached (active child limit: N); wait for an existing child to finish or complete this work with the current agents` [activation.ts:24-35]；深度=父+1、持久头为单调下界 [subagent/src/depth.ts:28-36]；超帽 → `subagent depth D exceeds maxDepth M` [child-agent.ts:32-37]；数字 maxDepth 需 provider depthLimit 能力否则装载失败 `provider "X" cannot enforce maxDepth (no depthLimit capability) — set maxDepth: 'provider-managed'…` [tool-subagent/src/index.ts:70-78,166-171]；到帽 agent 获模型上下文 `Delegation depth for `subagent`: current D, maximum M, remaining levels 0. Do not call…` [index.ts:382-395]
- preset 门控：请求需 agentPreset 能力且 !inheritsParentContext 否则 UNSUPPORTED_CAPABILITY "does not support independent agent preset selection" [subagent/src/index.ts:631-637]；缺 agent-presets 服务 → PRESET_UNAVAILABLE [child-agent.ts:191-195]；新建 append agent-preset/selected + subagent/descriptor [manager.ts:886-891]；省略继承父 preset；独立选 preset 的子冷续需 preset 投影否则 NOT_RESUMABLE [manager.ts:1358-1362,1486-1493]
- 委托策略：子审批钉 'never'、父沙箱 override+Auto/Full-access preset 记 source:'delegation' 事件 [child-agent.ts:221-279]；子获固定上下文 "You are a delegated subagent: your permission scope was fixed…" [child-agent.ts:171-175]

## 报告 6：压缩系统 / MCP 客户端（2026-10-21）

### A. 压缩

- 触发数学：thresholdRatio 0.8、headroomTokens 65536、retainRatio 0.16、compactionRetries 1、maxOverflowRetries 1、auto true；maxTokens 默认=headroomTokens [compaction-basic/src/config.ts:20,75-76,105-108]
- 分母=adapter 上下文窗（ctx.llm.resolveModelInfo）[index.ts:304]；messageBudget=contextWindow−reservedCompletionTokens（reserved=routed requestHeader maxTokens ?? adapter defaultMaxTokens ?? 0）[config.ts:172; index.ts:65-68]；pressureBudget=messageBudget−headroomTokens [config.ts:181]；thresholdTokens=floor(min(contextWindow×thresholdRatio, pressureBudget)) [config.ts:191-194]；缺上下文窗 → TargetPressureConfigError 每 provider/model 警告一次后跳过 [index.ts:167-173,307-313]
- 自动触发挂 agent/pre-step 调 compactIfNeeded('pressure') [index.ts:158-176]；仅 measured totalTokens ≥ thresholdTokens 时触发 [index.ts:319]、剪枝后复查 [index.ts:327]；modelPolicies 精确覆盖、重复目标加载失败 [config.ts:249-261]；pre-step 压缩失败警告 `step compaction failed: <msg>; continuing the turn` 继续 turn [index.ts:172]
- 摘要请求：[表面节点 0 为 system/message 时含 system head] + 影子区消息按表面序 + 会话工具（自 request header）+ 恰一条 user 消息全文=COMPACTION_INSTRUCTION [region.ts:544-563; summarizer.ts:225-231]；提示词逐字在 summarizer.ts:112-147（"You are now acting as a compaction engine for this AI coding assistant. Condense the conversation ABOVE into a structured checkpoint..."，9 个固定 ## 节：Primary Request and Intent / Key Technical Concepts / Files and Code / Errors and Fixes / Pending Jobs / Current Work / Next Step / Critical Context，含合并非复制既有 <compacted-summary> 规则）
- 调用 ctx.llm.stream purpose 'compaction'、maxTokens=config.maxTokens [summarizer.ts:232-243]；目标=配置 summarizationProvider/Model 对 > 最新 routed header > agent options；全无 → "no provider/model available for summarization: ..."；对必须成对设置 [config.ts:307-327]
- fail-closed：finish error/aborted → LlmError；max-tokens → 'MAX_TOKENS' "summarization truncated at the token cap (incomplete checkpoint)" [summarizer.ts:277-291]；image 输出拒 UNSUPPORTED_CONTENT；空文本 → "summarization produced no text summary content"
- 替换 user 消息=CHECKPOINT_PREAMBLE（"This is an automatically generated checkpoint condensing an earlier span..."）+ <compacted-summary> 包裹 [summarizer.ts:102-103,150-151,268-274]；收缩门：框架后估算须 < 影子区 tokens 否则 "summary is not smaller than the shadowed content (N estimated framed tokens >= M)" [region.ts:417-421]；摘要错误经 waterfall 'compaction/summary-error'（recover 默认 false）重试时重备输入 [index.ts:442-448]
- 保留：retainTokens=floor(messageBudget×retainRatio)；retainRatio ≥ thresholdRatio 拒绝；resolved retainTokens ≥ thresholdTokens 拒绝 [config.ts:195-204,235-240]；选区 selectCompactableRange [region.ts:117-155]：永不含 node-0 的 system/message（firstIdx=1）、自尾累积至 retainTokens、边界回退到 toolPairingBalancedBefore 使 assistant tool-call/result 对不拆分、无可压缩返回 null
- 溢出恢复：compactionRetries（重选区最多 1+ 次，仍超 → "compaction still above threshold after N compaction attempts (X estimated tokens >= threshold Y)"）[index.ts:329-346]；maxOverflowRetries 挂 agent/request-error 码 CONTEXT_WINDOW_EXCEEDED [llm/src/error.ts:25]：剪枝+retainTokens 0 强制压缩 [index.ts:294-302]，成功 {kind:'retry'} 计数 [index.ts:231-233]；计数在 idle 与每 assistant/message 重置 [index.ts:178-188]；耗尽透传原错误 [index.ts:200]；表面已收缩后压缩失败仍重试 [index.ts:212-219]
- 持久括弧：compaction/start … compaction/summary + replace-surface user/message … compaction/end；失败 close 留可检测未配对 start [region.ts:210-251,471-522]；摘要后全表面稳定性复查 [region.ts:431-440]
- tool-result-pruner：只触 tool/result 表面事件 [index.ts:254-261]；度量=文本块 Unicode 码点、非文本 0 [index.ts:186-192]；total > thresholdChars 才剪；移除段=[headChars, total−tailChars)；标记只在首个相交块插一次；非文本块保序 [index.ts:205-224]；替换须更小且 ≤ threshold [index.ts:236-238]；替换文本逐字 `\n\n[... tool result middle pruned ...]\n\n` [config.ts:7]；校验 head+marker+tail ≤ threshold [config.ts:55-63]；每次替换 append compaction/prune 影子价事件 + surfaceOp replace tool/result（sourceEventSeqs=[seq]）[index.ts:278-289]
- spill：maxInlineTokens 省略=禁用 [spill-policy/src/index.ts:48-50]、产品 12500 [cordis.patch.yml:414-417]；挂 tools/post-execute（accepted 结果，跳过 exec.name==='read' 与纯文本 PTC 子调用）[index.ts:133-140]；定价=estimateContent(text)+每图模型 imageRequestPricing [index.ts:57-75]；超预算全文落盘（图片渲染 `\n[Image: <readonlyPath>; <mediaType>; <w>x<h>. Use read_image to view it.]\n`）、suggestedName `<toolName>.txt` [index.ts:78-106]；模型面替换=头 + `\n\n[...]\n\n` + 尾 + footer `(Omitted N bytes.[ Omitted M images.] Full formatted result stored at: <locator>. <retrievalHint>)` [index.ts:41,108-118]；预算=预留最坏 notice、余量对半头尾、图片不可分、surrogate 安全 [retention.ts:75-163]；失败警告 `spill-policy: ...; keeping the inline content` 保留内联 [index.ts:127-130]
- spill-local 存储：`<root>/session-<sha256前12hex>/<12hex随机>-<encodedName>`、文件 0600 目录 0700 [store.ts:78-131]；默认 root=OS tmp 下 mkdtemp `dsh-spill-XXXXXX` [store.ts:16,36-39]；启动清扫删 mtime 超清理期（默认 30 天、0 禁用）、剪空目录、跳过符号链接 [spill-local/src/index.ts:48,66-117]
- /compact：仅空闲 agent（runMaintenance；busy → "manual compaction requires an idle agent with no waking queued work"）[compaction-basic/src/index.ts:390-434]；选区 retainTokens 0；独立标记对 + ctx.sessions.flush [index.ts:407-412]；null → 'No compactable history yet.'；成功 `Compacted N history items (~M tokens).`；错误文案 busy/cancelled/changed/summary/commit/persistence 逐字 [command-compact/src/index.ts:24-56]

### B. MCP 客户端

- 每服务器一插件实例；transport stdio（command/args/cwd，子环境=净化父环境+config.env）| streamable-http（url/headers）[index.ts:119-142; transport.ts:22-36]；serverName `^[A-Za-z0-9_-]{1,32}$` [index.ts:40]；活跃重名装载失败 "mcp-client: serverName \"X\" is already in use..." [index.ts:162-176]
- apply 阻塞于 connection.ready（首连+同步，成败都返回）[index.ts:199]；failOnStartupError 默认 false；true → 抛 `mcp-client(<server>): initial connection or tool synchronization failed` [index.ts:128,199-202]；首次同步 registrationFailure 仅 failOnStartupError 时 throw、之后全 contain [connection.ts:111-113]
- 客户端身份 {name:'dsh-mcp-client',version:'0.0.1'}、versionNegotiation auto、tools listChanged → 重同步 [connection.ts:231-242,368-376]；dispose 停重连、关 transport（5s 确认 GENERATION_CLOSE_TIMEOUT_MS=5000）、等在途尝试+同步链、注销全部工具 [connection.ts:127,173-182,361-380]
- 服务器指令=`### MCP server: <serverName>\n\n<trim 文本>` 或 ''；超 maxInstructionBytes（默认 32768）该次尝试失败 [connection.ts:192-195]；发布为 systemPrompt section `mcp:<server>` order MCP_SERVERS、interpolate:false [server-context.ts:19-27]
- 重连：默认 enabled true、initialDelayMs 500、maxDelayMs 30000、maxAttempts 10 [connection.ts:114-119]；校验拒绝未知键、延迟正有限 ≤2147483647、initial ≤ max、maxAttempts 正整数 [connection.ts:142-166]；延迟=min(maxDelayMs, initial×2^(failedAttempts−1)) [connection.ts:209]；预算仅在连接存活 ≥ maxDelayMs（稳定窗）后掉线才重置 [connection.ts:195]；超 maxAttempts → 注销全部工具、清指令、日志 `giving up after 10 consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect`，仅插件重载可再连 [connection.ts:198-207]；enabled=false：错误日志、工具持续失败或永不注册 [connection.ts:183-191]；失败尝试 5s 内无法确认 transport 关闭 → 停止重连 "failed generation could not confirm transport closure..." [connection.ts:161-170,405-412]；断连期间工具保持注册、资源请求抛 `mcp-client(<server>): server is disconnected`、死代在途调用被 isCurrent 作废 [connection.ts:133,342-350]；重连成功日志 `reconnected and re-synced tools (attempt N/M)` [connection.ts:315]
- 工具调用：toolCallTimeoutMs 默认 60000 随执行信号 [index.ts:37; tools.ts:138-141]；超时=Error → isError 结果；MCP isError 结果 throw 提取文本；非法 CallToolResult → `Tool "<raw>" returned an invalid MCP result: <issues>`；task-required 工具 → `Tool "<raw>" requires task-based execution, which this bridge does not support`；非对象模型参数坍缩 {} [tools.ts:179-198,183-186]
- 注册：公名 `mcp__<server>__<raw>` 归一到 64 上限 `[A-Za-z0-9_-]`，有损变更追加 `_<12hex sha256("server\0raw")>` [tools.ts:48-87]；raw 名仅上 wire tools/call；description/inputSchema 透传、outputSchema 仅支持时保留；canonical 输出 {content,structuredContent?} [tools.ts:131-142,250-266]；同步两段式（先取全代，失败旧代不动）；raw 重名 → `server listed tool "X" more than once — invalid tool list`；换装先 dispose 旧再注册新；注册表冲突回滚该服务器全部工具并 contain（或启动时 throw）[tools.ts:89-161]；list_changed 串行重同步 cacheMode 'refresh'
- 资源工具三件每 scope 注册（≥1 provider 时）：list_mcp_resources / list_mcp_resource_templates / read_mcp_resource；参数 server(req)/cursor(opt)/uri(read 必填)；输出 schema {type:'json'} [mcp-resources/src/tools.ts:34-52,144-190]；渲染逐字 `MCP server: <server>\n<JSON>`，blob 字段替换 `[binary resource: N base64 characters; available to programmatic callers]` [render.ts:16-24]；系统提示词 section 'mcp-resource-servers'：`## MCP resource servers\n\nUse list_mcp_resources, list_mcp_resource_templates, or read_mcp_resource with one of these names as the server argument: ["a","b"].` [index.ts:59-71]；未知/越权 server → `MCP resource server "X" is unavailable in this agent's scope` [index.ts:122-125]；资源操作共用 toolCallTimeoutMs
- 内容投影 [tools.ts:449-511]：text 块 '\n' 连接；resource_link → `Resource link: <name> (<uri>)`（缺名/uri → `[resource link unavailable: the MCP block is missing its name or URI]`）；audio → `[audio result unsupported: <mime>; raw audio data remains available to programmatic callers]`；embedded resource → `[embedded resource unsupported; raw resource data remains available to programmatic callers]`；未知类型 → `[unsupported MCP content type: <type>]`；非对象块 → `[unsupported MCP content block: expected an object]`；空内容 → `(<toolName> returned no model-visible content)`；图片仅 PNG/JPEG/WebP/GIF canonical base64 且模型路由声明 image input 才准入，拒绝投影 `[image unavailable: <mime>; <reason>; raw image data remains available to programmatic callers]` 而 canonical 值保留原始数据 [tools.ts:57-65,307-379]

## 报告 7：装配/patch 语义与其余插件行为（2026-10-21）

### A. 装配与 patch

- 层序：bundle 层按 dsh.profile.bundles 序盖空 entry 表 → profile 自身 cordis.patch.yml → --patch 叠加 → telemetry 开关；home 层 $DSH_HOME/cordis.patch.yml 高于 per-profile 层（machine-local 优先）[apps/cli/src/profile-boot.ts:9-13,225-231,253-254; packages/boot/app-boot/src/profile.ts:7-14,769-772; profile-context.ts:74-83]
- 根配置每 boot 重写为固定空 []（仅为 Loader 提供 baseUrl 锚，防写回组合行重复 insert）[profile-boot.ts:214-227]；组合=单次 applyEntryPatches([], layers.flat()) [profile.ts:816-823]
- patch 语义 [vendor/include/src/index.ts]：输入克隆不变异（57-63）；按 id 索引含 group 子项（65-74）；带 id insert 追加进目标 group，目标须存在且为 group 否则警告跳过（79-91）；裸 insert 追加根（92-94）；插入行被索引供后续 patch 定位（95-100）；无 id 非 insert 警告（104-107）；未知 id 警告 "patch: entry %C not found" 跳过永不致命（109-113）；name 不符跳过警告（115-118）；其余键覆盖目标行（120-123）；目标行缺失保持逐条警告 [app-boot/src/index.ts:570-575]
- !!js 表达式：YAML tag 构造 {__jsExpr}、entry 激活时各 fiber 上下文求值；Group/Include 树载体保持字面 [vendor/loader/src/index.ts:104-113; include 158-167]；dump 原样打印未求值
- insert 行相对路径锚定 patch 文件旁转 file URL [app-boot index.ts:539-548]；重复 id 无错：EntryGroup.create 复用既有 Entry（store[id] ??=）、update 替换 options——后行胜出、插件以新 options 重启 [vendor/loader/src/config/group.ts:21-26; entry.ts:118-133]；ensureId 仅缺省时生成、显式冲突 id 保留 [tree.ts:51-58]
- 缺依赖：inject 服务缺失 → fiber PENDING（loader [Service.check] false）[loader index.ts:175-179]，审计报 pending (waiting for services: …)；optional 容忍、required 致命 [app-boot index.ts:1073-1077,1155-1168]；import 失败：Entry._init 捕获、logger.error、无 fiber [entry.ts:223-238] → "failed to import"，optional 容忍
- 启动策略：仅 bootstrap Include 与全局 required id 集 {agent-loop, webserver, web-runtime, modules, connection, headless-runner, acp, sdk-jsonrpc-server} 经 StartupError 拒启动（列 N required plugins did not activate + Failed/Waiting 节）；其余一律单警告兄弟继续；required 缺失/禁用被忽略；disabled 表达式抛错计失败 [app-boot index.ts:944-961,1092-1114,1150-1168]。**failOnStartupError 作为命名配置键不存在**；策略恰为此 required-id 审计
- patch 清单文件：顶层 YAML 数组必需；可选文件缺失=无该层，不可读/不可析/非数组=throw；overlay（bundle/profile cordis.patch.yml）缺失 throw [app-boot index.ts:509-536,552-565]
- 跳过的 bundle：resolution/manifest/compat/patch 失败收进 skippedBundles、stderr 一次 `skipping profile bundle "x": reason`，永不致命；profile manifest 错误与用户 patch 错误仍 throw [profile.ts:105-122,740-781]
- env 分层：继承进程 env > 项目 .env（cwd）> home .env（$DSH_HOME/.env）；已设名不被替换；bootstrap 专名（PATH、NODE_*、代理、DEEPSEEK_BASE_URL、全部 DSH_*/XDG_* 前缀等）在任何 .env 拒绝，home 层例外可设 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY [app-boot index.ts:126-260]
- DSH_HOME 优先序：configured > $DSH_HOME > ~/.dsh；空白 $DSH_HOME 视为未设 [util/home-paths/src/index.ts:126-137,12-18]
- include 热重载：解析失败保上一好树警告 "config reload at %C failed"；仅 ENOENT 回退 initial；空/非数组 throw "must be a top-level array"；原子 tmp+rename 写、EACCES/EBUSY/EPERM 重试×10 [include 213-308]；纯 Volatile 配置变更原位提交不 remount [entry.ts:142-151]
- 配置 dump：base+层 YAML、`# == <file>[, patched by …]` 连续来源注释、逐层警告位置扩展 [app-boot index.ts:611-713]
- Recompose（热重载）：新代弃行移除（新代失败按原索引还原）、failed-import 重试、"introduced" 失败（新增/变更/required/曾运行）throw；pending-service 条目不算 introduced [app-boot index.ts:328-440,399-403]

### B. 插件行为

- repeat-tool-reminder [guard/repeat-tool-reminder/src/index.ts]：默认 thresholds [3,5,8]、argumentsPreviewChars 500（39-52）；配置响亮失败：空/非整/\<2/重复阈值、preview \<1 全 throw（175-204）；检测键=JSON.stringify([toolName, canonicalArgs])，canonical=深键排序 JSON（117-137）；post-execute 计数故被拒调用也计（206-213）；exclude/include 外工具透明不计数不重置（154-157）；阈值时以 additionalContexts 前插 user 消息（accept 与 block 都发，207-224）；首档 GENTLE_REMINDER 文案（66-73）、后续档 DETAILED 文案含 tool/consecutive_calls/arguments（79-90）；preview 头截断 `… (+N more chars)`（140-146）；source {kind:'repeat-tool-reminder',form:'notice',summary:"<tool> × <count>"}；step 内 user 消息重置该 agent 链（227-231）；从不否决或改写调用；需 exec.agent（直接 execute 不计）
- jobs-local [jobs-local/src/index.ts,pump.ts]：默认 maxConcurrentJobsPerOwner 10、retainBytes 262144、settledRetainBytes 16384、pumpPollMs 150（34-43,131-152）；超 per-owner 上限 throw "background job limit reached …"（220-225）；状态 running → stopping → completed|killed|failed（先到先结算，603-604）；仅 terminal 可移除（340-344）；terminal 后 kill 返回 'already-finished'（484-485）；killed 结算合并记录的 kill reason（606-616）；输出环：结果随首个 terminal 读、terminal 读裁到 settled 帽但不低于未消费字节（470-482,624-628）；settled 后 append 丢弃警告（537-551）；producer done 拒绝 contained → failed（284-291）；拉泵按源数组序每 pollMs 排空、结算后再排空一次；有损读落 gap chunk [pump.ts:44-83]；失败源记一次后读作耗尽（558-573）；首个 job 事件恒为 registered 且先于泵启动播发（276-280）；owned job 仅同 session 可达 "belongs to another session"（398-414）；owner agent 处置取消+等待+丢弃 owned jobs（644-660）；teardown cancel 抛错强判 failed "work may be orphaned"（694-714）；生命周期记录 session 事件 job/registered 与 job/settled（444-463）
- schedule [schedule/src/runtime.ts,domain.ts]：到期活动任务逐个解析 agent、agent.followup（source kind 'schedule'）后必须 sessions.flush 确认否则保持到期并警告 "were not acknowledged" [runtime.ts:101-152]；消息 one-shot `[SCHEDULE REMINDER]` 框架（schedule_id_json/occurrence_at/reminder_prompt_json）[domain.ts:1868-1876]、同会话 recurring 兄弟合批 `[SCHEDULE REMINDER BATCH]`（reminders_json）（1883-1896; runtime.ts:114-117）；补发规则：recurring 决策解析决策时不晚于的最晚到期发生、不枚举错过的（every 地板算术 786-821；daily/weekly latestDueOccurrence=max(candidate, committed scheduledAt) 849-952）；下一目标=交付日后首个严格未来本地发生（836-856）；时区仅 UTC 或 IANA Area/Location、Intl 规范化（370-389）；DST 空洞时刻 one-shot 拒绝 "does not exist in the selected time zone"（427-434）、recurring 扫描不产生发生（421-425）；DST 歧义取较早时刻；定时器至多一个 unref'd、钳 2147483647ms、启动/变更/失败后重算、失败 id 排除出下次计时 [17,144-164]
- attachment-local [attachment-local/src]：默认 maxImageBytes 20MiB、maxImagesPerMessage 20、maxMessageImageBytes 200MiB、maxImagePixels 64000000、maxImageDimension 8192、媒体类型 png/jpeg/webp/gif（33-58,178-185）；错误码/文案：空图 INVALID_IMAGE "Image is empty."、类型不符 IMAGE_TYPE_MISMATCH、超限 IMAGE_TOO_LARGE "Image exceeds the configured byte limit."、解码像素 IMAGE_TOO_MANY_PIXELS "Image exceeds the configured decoded-pixel limit."、持久化失败 ATTACHMENT_WRITE_FAILED "Unable to persist attachment."、摘要不符 ATTACHMENT_CORRUPT（store.ts:61-106,198,288-300; image.ts:122）；更大源准人并降采样 2048²/8192 长边/4MiB 预算、质量梯最小可用输出（43-54）；并发 1-8 越界 throw（191-198）；同请求图变体共享在途变换（256-287）
- credentials-local：文件 $DSH_HOME/.credentials.yaml；缺失=空 store；存在但非法 fail 激活（809-827）；chokidar watch（默认开、debounceMs 100、awaitWriteFinish）热发布；刷新失败警告保上一好文档（581-618,846-856）；reconcile 差异化仅通知变更键（861-897）；未知顶层键按名警告不涉值（899-908）；解析优先序：stored 文件值 > 继承进程 env（空值视为未设）> .env 层（项目后用户，"below the managed store, never above it"）（744-767）；写：mkdir 0700、文件锁、对盘读改写、原子 0600；空值拒绝（"use unset"）（747-781）；每次重载/写前复查 owner-only（862-864）；旧平铺布局锁下原位迁移（832-867）
- time-context [context/time-context/src/index.ts]：前置监听器，非拒绝 step 后且出刷新窗（默认 600000ms 间 durable 注入；0=每合格 step）append user 消息 source {kind:'time-context',form:'snapshot'}（185-225）；文本 `Time sampled while preparing turn T, step S: <格式化时间戳>\n<浏览器时区上下文>\nElapsed since the preceding <model-visible message|step context>: <elapsed|unavailable>.`（100-115）；elapsed 紧凑 `1d 2h 3m 4s`（69-84）；时区=开启 turn 消息唯一浏览器时区 > config timeZone > 进程时区；非法时区装载失败（133-155）；投影 stateVersion 2
- working_directory [tool-working-directory + working-directory]：无参读、cd 改；输出仅 cwd 串；需 agent session 否则 throw；已有 shell/进程保留自身目录；服务 per-session durable 投影：ensure 在当前目录消失时还原原始项目（会话头 cwd，否则 defaultDirectory=config 或 process.cwd，须绝对）、承诺先于兑现提交（122-152,75-79）；set 校验存在并提交；空 cd 拒绝（163-184）；变更注入 'working-directory' 来源通知 "The working directory changed from X to Y."（恢复变体措辞 186-206）；system-prompt context "Current working directory: \"…\"." 组装时保鲜、缺失 unshift（90-110）；操作逐会话串行（208-222）
- user-questions：ask 拒空问题（EMPTY_QUESTIONS）、仅确切 live root agent 可供（CALLER_NOT_LIVE/DELEGATED_CALLER）、意图校验 BAD_INTENT、无应答者 NO_PROVIDER、经 Agent 域 waterfall（291-330）；askTimed：timeout 安全整数 1..2147483647（BAD_TIMEOUT）、重复前台等待拒（DUPLICATE_WAIT）、客户端超时映 {pending:true,callId}、处置以 ASK_ABORTED 关前台等待（234-269,89-94）；answer（续问题）：回复以 user 消息 steer，内容 canonical JSON {kind:'answer_to_pending_question',tool:'ask_user_question',callId,questions,answers}；未逐题点名 → BAD_ANSWER；重复回复 REPLY_QUEUED（164-206）；先于 claim 的回复排队、discard/turn-end 释放（95-129）
- session-title：fallbackMaxWords 5、fallbackMaxBytes 40、maxTitleBytes 80（cordis.patch.yml:60-65；schema 正整数）；fallback：首个合格人类 prompt → 剥 OSC/CSI/ESC/控制/方向字符、折叠空白、前 5 词、UTF-8 安全截 40B [normalize.ts:11-77]；source {kind:'fallback'}；空结果不 append；用户改名钉住标题（自动修订不覆盖 kind:'user'，494-496）、显式 refresh 唯一解钉（434-446）；LLM 路径：单可选 provider、cadence 'first-prompt'|'all-prompts'（506-514）、仅在确切主请求路由被记录后启动（537-560）、更新用户消息取代待发生成（515-517）、处置中止在途调用且迟到结果不提交（474-499）；接受标题归一 maxTitleBytes（645-681）；改名空 → SessionTitleInvalidError（87-93）；标题 log-only 永不进模型表面（73-77）
- token-meter：不接受设置、未知键 throw "unknown key"（87-104）；按会话重放 durable log；measure 返回冻结 {logRevision,baseline,surfaceDeltaTokens,totalTokens,surfaceTokens,nodes}（146-191）；baseline：provider usage 仅当锚定调用 canonical header 匹配且 usage ≥ 该调用全路由定价锚才复用否则估算；空会话 {kind:'none'}（158-181）；锚在 assistant/message 捕获（前置表面+header）（292-320）；usage total=input+cacheRead+cacheWrite+output 无 reasoning 双计（70-76）；图片节点按路由模型 adapter 定价或固定启发（193-198）；畸形事件序 throw（268-298）；image/offload 标记表面图片卸载（252-264）
- plan-mode：部署配置仅 {section}；缺/空/非串/未知键装载 throw（98-116）；活动或 pending 期间渲染 section 为 system-prompt 段 plan:policy（242-249）；模式变更在开放 turn 内下一被接受 in-turn pre-step 生效、turn 间立即；set 返回 committed|queued|cancelled|noop（381-413）；旁白通知 "The user switched this session to plan mode." / "…back to the default mode." 仅当最后 request header 为另一模式（421-434）；exit_plan_mode 非活动仍注册但 throw "only available in plan mode"（330-332）；plan 须 # 标题开头（333-335）；评审经 user-questions 意图 plan-review、approve/keep-planning 标签（337-360）；评审被解（ASK_CANCELLED）→ turn 以 approved:false 结束（361-368）；keep-planning 抛错带 feedback 文本（376-382）；批准排队静默选择于下一 pre-step 施加（384-388）；评审中服务重载 → 显式 throw（369-372）
- goal runtime：自动轮仅 phase 'active'+activation 'armed'（106-108）；轮数 roundsStarted+1、source {kind:'goal',goalId,revision,round} round>0（50-52,190-195）；roundsStarted ≥ maxGoalRounds → goals.block 码 'round-limit' "Goal reached its configured limit of N rounds."（182-188）；blocked 亦在 queue 失败（'queue-failed'）与 pre-step 拒绝（'prompt-rejected'）（209-220,419-430）；host 发起暂停中止运行 turn（agent.cancel({kind:'user'},{keepInbox:true})）、模型发起暂停正常收尾（302-313）；排队未领取轮 idle 撤回、暂停 fence 到确切 attempt revision（274-300）；max-tokens turn 结束解武装；aborted turn 取消已 claim/admitted 尝试否则解武装（338-362）；pre-step 拒绝的轮 prompt 还原其他已 claim 消息后重排（393-402,437-444）；在既有 agents 上装载 driver 永不继承自动权威——全部解武装（447-452）；GOAL_CHANGE_VERSION=1
- session-query sqlite：FTS5 虚表 persisted_docs 与内存 temp.live_docs [schema.ts:127,158]；高亮标记 U+FDD0/U+FDD1 输出剥除（query.ts:18-21,228-236）；调用方文本引为单 FTS5 短语使查询语法惰性（219-226）；限制 defaultLimit 20、maxLimit 100、snippetChars 240、readWindowMax 50 [index.ts:79-83; config.ts:6]；limit 出 1..max → SESSION_QUERY_INVALID_LIMIT "must be an integer between 1 and N"（369-382）；绑定变量 >32766 → portable-limit 错；外层谓词 >14 → FTS5 预算错（24-52）
- 缺席事实：无全局 failOnStartupError 键（策略=required-id 审计）；DSH_PERMISSION_MODE 无 TS 源读取（仅作为子代理 env 合并转发 ambient 事实 [subagent-acp/src/run.ts:40-50]；SandboxMode 'danger-full-access' 是 config 级枚举非 env 解析 [sandbox/src/index.ts:27-33]）；patch 无 dedup——重复 id 后行胜出

## 报告 8：TUI 可观察行为（2026-10-21）

**基线事实：TUI 包已从 dsh HEAD 删除**（commit 10bb9cbf4a "cleanup: remove TUI package and legacy dsh entrypoints"，位于 packages/ui/tui，基于 pi-tui 0.80.7）；以下 file:line 取自其最后版本（git show 10bb9cbf4a~1）。fly-rafter M2 TUI 对标此最后版本，需用户确认基线。

- 启动：launcher 原样传内参 [apps/cli/src/args.ts:8-14,46-56]；硬门：stdin 与 stdout 必须皆为 TTY、无管道回退 [src/index.ts:1970-1971]；无备用屏（inline 差分渲染，pi-tui）[index.ts:335]；首渲染 banner 左到右 24 帧 15ms 扫入 [helpers.ts:145-149; index.ts:1818-1843]；副标题=configured welcome，会话标题落日志后换标题 [README.md:15; index.ts:414-415]；窗口标题 `<session title> — <configured title>` [index.ts:510]；启动失败全屏前打印并 exit 1 [index.ts:1931,1935]
- 全局键（无 overlay 活跃时）[index.ts:1679-1714]：Ctrl+O 工具卡 collapsed→expanded→hidden 循环；Ctrl+R 推理块开关；Ctrl+L 全重绘；Escape 取消运行中 turn；Ctrl+C 运行中取消/空闲清空输入/空闲且空输入退出；Ctrl+D 空闲退出否则警告通知
- 编辑器：Enter 提交、Shift/Alt+Enter 换行、Up/Down 提示历史（仅内存不持久）[index.ts:1627,1662]；提交路由：/ 前缀→命令、/skill:<name>、@[session] 引用 [index.ts:1621-1650]；运行中 steer、空闲 followup [1453-1460,1513]；会话引用 prep 失败还原输入并禁重复提交 [1662-1675]；粘贴/IME 由 pi-tui 承载（硬件光标可选默认关 [config.ts:63-64]）
- 补全：光标行 0 且光标前文本以 / 开头无空格触发 [index.ts:1312]；Tab 转发 \t [1316]；合并提供者=斜杠命令+@ 文件/目录+会话引用 [chat/autocomplete.ts:21-66]；@ 词法 @"quoted path" 或裸 @path 非词中 [file-autocomplete.ts:59-75]；目录保持补全开启供下钻、插入带尾空格、空格引号化 [file-autocomplete.ts:29-34]；上限 20 结果/10000 索引条目、排除 .git/node_modules [file-autocomplete.ts:12-16; config.ts:57-62]
- 转写渲染：用户 prompt 粗体下划线 You 头+Markdown 体 [transcript.ts:10,177-216]；reasoning 暗淡斜体（213）；assistant 步流单组件+步计时脚注 [index.ts:720-758]；diff 卡 +/- 行着色计数、暗淡上下文、超 maxDiffEditLength（默认 1000）整侧近似 + `[exact line diff omitted: >N changed lines]` 行 [transcript.ts:79-114; config.ts:37-38]；create 模式全行计 added（85-89）
- 卡片族（presentCall/presentResult）：terminal（$ cmd + cwd 行、头 `/ <desc>`）、diff、generic、read、search、web [transcript.ts:470-500,533-537,579,590-670]；折叠预览 `… +N lines (Ctrl+O to expand)`，maxToolOutputLines 默认 6 [transcript.ts:553-564; config.ts:35-36]；hidden 模式每 turn 折一个 Assistant 头+无头续 [transcript.ts:699-715]；压缩插暗淡 `… earlier context was compacted …` 标记 [index.ts:275,1749-1751]
- 动画：状态字符替换 > 光标——◍ 模型等待/✻ 思考/● 响应/⚙ 工具/⊙ 压缩中 [chat/timing.ts:207-215]；50ms tick、300ms fade、1400ms throb [timing.ts:18-28]；状态行=cwd(git 分支)、模型标签、↑in ↓out usage+cache N%、N% context、N queued 徽章、dsh 符号、压缩行 `Context being compacted <elapsed>` [index.ts:440-458]；相位标签 Model wait/Thinking/Response/Tools <elapsed> 连接 · [timing.ts:69-76,352-355]
- 审批 UX：权限/提问=ctx.userInteraction 问题渲染为转写与编辑器间内联 QuestionDialog [components/dialogs.ts:806-1084]；与插件 overlay/模型选择器共享 FIFO [extension/overlay-manager.ts]；键：Up/Down 选项、Space 多选切换、Enter 提交、Tab/c 自定义作答、Esc/Ctrl+C 取消、PageUp/Down 长内容翻页 [dialogs.ts:847-884]；校验错误 `Select at least one option…` / `Enter an answer before submitting.`；↑ N more/↓ N more 隐藏行标记、i/N 位置、编号进度头 [972-1019]；abort 表现为 ask_user interrupted 错误 [README.md:159]
- overlay/popups：/help、/model 选择器（过滤框、↑/↓、Shift+Tab 循环 reasoning efforts、Enter 选、Esc 清过滤再关）[dialogs.ts:281-425; index.ts:1363-1368]；/clear 仅视图 [1372-1374]；/details [collapsed|expanded|hidden] [reasoning [on|off]] 居中切换对话框、Tab 循环即应用、Enter/Esc/Ctrl+C 关 [index.ts:1160-1188; dialogs.ts:444-490]；/palette 色角色卡 [1383-1385]；/reload dev-only 空闲重读 [1388-1390]；/resume 全视口选择器：输入即搜、↑/↓+Page、Tab 切 workspace/all-workspaces、Enter 续用、Esc 清/取消、Ctrl+C 取消、live/当前会话行禁用 [dialogs.ts:569-691; chat/resume.ts]；/status 诊断卡（session id/title/dir/model+effort/agent 状态/计数/token 桶/KV cache/context/created/active）[index.ts:1217-1268]；/exit+/quit 同 handler 等空闲 [1407-1414]；/skill:<name>
- resize：无 dsh 级 handler（pi-tui 承载）；鼠标：absent
- 退出：Ctrl+C（空闲空输入）或 Ctrl+D 或 /exit、/quit 等空闲；退出路径 dispose root → ui.stop() 还原终端 → 原样打印 goodbye 行（暗淡）→ exit(0) [index.ts:1037-1057]；卡住的 disposer 5s 兜底 [1940,1959]；会话启动失败 exit 1 [1931-1935]
- 非交互模式：TUI 无（TTY 硬门）；兄弟入口 dsh headless "<task>" 一次打印即出 [args.ts:56]
- 主题不变量：仅 16 色 ANSI SGR 单 paletteSpec 表（/palette 打印）；truecolor 例外=banner 渐变+品牌 #4D6BFE；选择=反视频、活跃问题行=粗体强调；color:false 剥样式；不可信文本经 C0/C1 hex 转义 displayText [components/theme.ts; text.ts; README.md:19,85-89]
