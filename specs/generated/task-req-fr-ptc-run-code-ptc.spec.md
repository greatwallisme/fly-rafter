spec: task
name: "run_code / PTC"
tags: [requirements, generated-draft, ptc, tools]
satisfies: [REQ-FR-PTC]
---

## Intent

run_code 是程序化工具调用（PTC）入口：TypeScript async 函数体（仅可擦除语法），由全新 Node 子进程以原生 strip-only 模式执行，全链路 1:1 对齐 dsh PTC 运行时。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- fr-ptc MUST 以全新 Node 子进程执行 TypeScript（strip-only 类型擦除：程序包成 async function __dsh_program__ 后经 stripTypeScriptTypes，再以 AsyncFunction('use strict') 求值）；会话状态不可读；文件系统访问 MUST 按沙箱策略执行（非禁止）；进程在沙箱策略执行世界中启动；沙箱 seam 不表达网络限制（如实承袭 dsh 事实）；语言 flavor 仅 TypeScript（对齐 TYPESCRIPT_FUNCTION_FLAVOR 文本）（dsh ptc-runtime-node/src/index.ts:30,48,200-203、bootstrap.ts:404-411）。
- wire 帧协议 MUST 对齐 dsh channel.ts/json-wire.ts/protocol.ts 的帧上限、有界写队列、流式 stdout、崩溃分类；实现于 fr-ptc（tokio + 子进程）。
- 失败分类全集 `exception | timeout | abort | worker-exit | invalid-output | output-limit | protocol | sandbox-unavailable`，全部 fail-closed；触发条件 MUST 精确：exception=strip 抛或程序抛；timeout=wall 计时器；abort=取消信号且非超时；worker-exit=先行退出/输出流失败/清理失败；invalid-output=最终值非 lossless JSON；output-limit=LogBuffer 预算/admit 溢出/ledger 超限重写；protocol=非法帧/坏 log/坏 call id（须严格递增）/未声明绑定/pending 双上限（128 个与 128MiB）；sandbox-unavailable=SandboxUnavailableError 或 runner spawn 规则（dsh ptc-runtime-node/src/index.ts:200-339、bootstrap.ts:47-96,176-182、output-ledger.ts:24-65）。
- 能力面：工具以全局 async 函数注入（`await tools.name(args)`），参数/返回值必须 lossless JSON；`console` 为保留 log 槽，输出计入共享字节预算，超限报 output-limit；`timeout-ms` 预算超时归类 'timeout'。
- 绑定名校验：可移植标识符，拒绝保留字与 `console` 槽；dunder 仅在 error 成员位拒绝（RESERVED_ERROR_MEMBERS），绑定 global 不拒 dunder；`__proto__` MUST NOT 拒绝——按普通 own key 注入（宿主 null-prototype 对象 defineProperty，child 同）（dsh bindings.ts:7-53、tools/src/ptc.ts:686-694、ptc-runtime-node/src/bootstrap.ts:313-358）。
- 声明生成：从当前注册工具 schema 生成 TS 绑定声明文本，经系统提示词 TOOLS_SDK section（order 5000）下发模型；工具按 name 字典序、键非裸标识符 JSON 引号、JSDoc 折叠转义 `*/`、additionalProperties!==false 追加 Record 重载、敌意 schema 降级 unknown；run_code 为 registry 保留传输（mode: ptc），不参与可过滤能力层（dsh tools/src/ts-types.ts:18-38,169-171,240-247）。
- 子调度：回流完整管线（含审批升级 approveEscalation），append `tool/ptc-dispatch-start` 与 `tool/ptc-dispatch`（parentCallId/subCallId 配对）；`max-parallel-sub-calls` 默认 10、submission-ordered；沙箱策略切换中止在跑程序。
- `execution-mode: persistent-in-memory`（opt-in）：每 state owner 一个受限持久 REPL 进程、单并发、换沙箱策略 MUST 先 release 旧进程再新建（dsh ptc-runtime-node/src/persistent.ts:69,125,334-356）。
- node 缺失时该工具调用 fail-closed 报错，系统其余部分零影响；Python 后端不在范围。
- 子调用协议 MUST：子调用 id 形如 parent 加 ptc 加序号且严格递增；pending 双上限（128 个与 128MiB）；dispatch-start 于真正获得槽位时追加；dispatch-log waterfall 听众抛错 contained 回退不传播；logWork 超过子并发上限应用背压排队；abort 时排队未启动者 abandon 不记日志（dsh ptc.ts:447-532,613-620,662、tools/src/index.ts:1331-1341）。
- 审批升级 MUST：permissions 与 justification 成对非空；同模式免审、升级须严格更宽（目标仅 workspace-write 与 danger-full-access）；rejected、cancelled、unavailable 一律抛错（dsh escalation.ts:41-63,165-217）。
- mode:ptc 下模型直接工具调用（非经 run_code 程序）MUST 在策略管线前坍缩终结：UNKNOWN_TOOL 加路由文本，绕过 pre-execute 与 guards（dsh tools/src/index.ts:1461-1482）。
- PTC 运行时默认值 MUST：execution-mode 'per-call'、timeoutMs 120000、maxTimeoutMs 600000、maxOutputBytes 67108864（64MiB）、maxOldGenerationSizeMb 512、maxMessageBytes 134217728（128MiB）、maxPendingCalls 128、graceMs 3000（dsh ptc-runtime-node/src/config.ts:31-44）。

## Boundaries

### Allowed Changes
- crates/fr-ptc/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: run_code happy path
  Test: pending_req_fr_ptc_run_code_happy_path
  假设 模型提交合法 TS 程序调用两个工具
  当 子进程执行完成时
  那么 返回结果为 lossless JSON，日志含 tool/ptc-dispatch-start 与 tool/ptc-dispatch

Scenario: 不可擦除语法 fail-closed 拒绝
  Test: pending_req_fr_ptc_fail_closed
  假设 程序含 strip-only 模式不可擦除的语法
  当 子进程装载程序时
  那么 fail-closed 报错且不执行程序

Scenario: 超帧上限归类 protocol
  Test: pending_req_fr_ptc_protocol
  假设 一帧超过协议帧上限
  当 帧解析时
  那么 该调用以 protocol 失败分类结算

Scenario: 大输出触发有界写队列背压
  Test: pending_req_fr_ptc_requirement
  假设 子进程 stdout 大量输出
  当 写队列达到上限时
  那么 应用背压，不无界缓冲

Scenario: 程序超时归类 timeout
  Test: pending_req_fr_ptc_timeout
  假设 程序超过 timeout-ms 预算
  当 预算耗尽时
  那么 失败分类为 timeout

Scenario: 子进程异常退出归类 worker-exit
  Test: pending_req_fr_ptc_worker_exit
  假设 子进程非正常退出
  当 结算时
  那么 失败分类为 worker-exit

Scenario: console 输出超预算归类 output-limit
  Test: pending_req_fr_ptc_console_output_limit
  假设 程序 console 输出超过共享字节预算
  当 结算时
  那么 失败分类为 output-limit

Scenario: 非 lossless JSON 值被拒
  Test: pending_req_fr_ptc_lossless_json
  假设 工具参数或返回值不可 lossless JSON 化
  当 程序调用该工具时
  那么 该子调用报错

Scenario: 保留名绑定被拒
  Test: pending_req_fr_ptc_requirement
  假设 程序声明绑定名 console、__proto__、dunder 或保留字
  当 绑定名校验时
  那么 拒绝该程序

Scenario: TOOLS_SDK 声明覆盖全部工具
  Test: pending_req_fr_ptc_tools_sdk
  假设 registry 已注册全部内置工具
  当 生成 TS 绑定声明文本时
  那么 声明覆盖全部注册工具 schema 且经 order 5000 的 TOOLS_SDK section 下发

Scenario: 子调度回流完整管线并触发审批
  Test: pending_req_fr_ptc_requirement
  假设 程序内子调用命中需审批操作
  当 子调度回流管线时
  那么 走同一审批 seam，未应答则 fail-closed

Scenario: 子调用并发上限 submission-ordered
  Test: pending_req_fr_ptc_submission_ordered
  假设 程序发起 11 个子调用且上限为 10
  当 并发提交时
  那么 第 11 个排队，完成一个放行一个

Scenario: 沙箱策略切换中止在跑程序
  Test: pending_req_fr_ptc_requirement
  假设 程序运行中权限模式切换
  当 切换结算时
  那么 在跑程序被中止

Scenario: persistent-in-memory 状态跨调用保留
  Test: pending_req_fr_ptc_persistent_in_memory
  假设 execution-mode 为 persistent-in-memory 且同 state owner 连续两次调用
  当 第二次调用访问第一次的变量时
  那么 状态保留可见

Scenario: node 缺失仅该工具报错
  Test: pending_req_fr_ptc_node
  假设 运行环境无 node
  当 调用 run_code 时
  那么 该调用报明确错误，系统其余部分零影响

Scenario: run_code 不参与可过滤能力层
  Test: pending_req_fr_ptc_run_code
  假设 配置了工具过滤能力
  当 应用过滤时
  那么 run_code 仍注册（mode 为 ptc 的保留传输），不可被过滤移除

Scenario: 程序 stdout 流式回传
  Test: pending_req_fr_ptc_stdout
  假设 程序持续输出 stdout
  当 程序未结束时
  那么 输出已流式回传宿主，不等待进程结束

Scenario: 子调用 id 与 pending 上限
  Test: pending_req_fr_ptc_id_pending
  假设 程序并发提交超过 pending 双上限
  当 host 结算时
  那么 超限以 protocol 失败，id 非严格递增亦然

Scenario: 升级路径判定
  Test: pending_req_fr_ptc_requirement
  假设 程序分别请求同模式、更宽模式与非目标模式升级
  当 approveEscalation 结算时
  那么 同模式免审、合法宽模式走审批、非法目标直接抛错

Scenario: 失败分类 exception 归类
  Test: pending_req_fr_ptc_exception
  假设 程序抛出异常或 strip 阶段抛出
  当 结算时
  那么 失败分类精确为 exception 且 fail-closed 返回

Scenario: 失败分类 timeout 归类
  Test: pending_req_fr_ptc_timeout
  假设 程序运行越过 wall 计时器预算
  当 结算时
  那么 失败分类精确为 timeout 且 fail-closed 返回

Scenario: 失败分类 abort 归类
  Test: pending_req_fr_ptc_abort
  假设 收到取消信号且并非超时路径
  当 结算时
  那么 失败分类精确为 abort 且 fail-closed 返回

Scenario: 失败分类 worker-exit 归类
  Test: pending_req_fr_ptc_worker_exit
  假设 子进程先行退出、或输出流失败、或清理阶段失败
  当 结算时
  那么 失败分类精确为 worker-exit 且 fail-closed 返回

Scenario: 失败分类 invalid-output 归类
  Test: pending_req_fr_ptc_invalid_output
  假设 程序最终值非 lossless JSON
  当 结算时
  那么 失败分类精确为 invalid-output 且 fail-closed 返回

Scenario: 失败分类 output-limit 归类
  Test: pending_req_fr_ptc_output_limit
  假设 LogBuffer 预算耗尽、或 admit 溢出、或 ledger 超限重写
  当 结算时
  那么 失败分类精确为 output-limit 且 fail-closed 返回

Scenario: 失败分类 protocol 归类
  Test: pending_req_fr_ptc_protocol
  假设 出现非法帧、坏 log、非严格递增 call id、未声明绑定或 pending 双上限
  当 结算时
  那么 失败分类精确为 protocol 且 fail-closed 返回

Scenario: 失败分类 sandbox-unavailable 归类
  Test: pending_req_fr_ptc_sandbox_unavailable
  假设 SandboxUnavailableError 或 runner spawn 规则命中
  当 结算时
  那么 失败分类精确为 sandbox-unavailable 且 fail-closed 返回

Scenario: __proto__ 绑定按 own key 注入
  Test: pending_req_fr_ptc_proto_own_key
  假设 程序声明名为 __proto__ 的绑定
  当 宿主构建绑定对象时
  那么 不拒绝，经 null-prototype 对象 defineProperty 以普通 own key 注入

Scenario: error 成员位 dunder 拒绝
  Test: pending_req_fr_ptc_error_dunder
  假设 程序在 error 成员位声明 dunder 名绑定
  当 绑定名校验时
  那么 以 RESERVED_ERROR_MEMBERS 拒绝；同名出现在绑定 global 位则放行

Scenario: 程序包装与严格求值
  Test: pending_req_fr_ptc_requirement
  假设 模型提交合法 TS 程序
  当 装载执行时
  那么 程序包成 async function __dsh_program__ 经 stripTypeScriptTypes 后以 AsyncFunction('use strict') 求值

Scenario: 程序 fs 访问按沙箱策略
  Test: pending_req_fr_ptc_fs
  假设 程序内子调用触发文件系统访问
  当 沙箱策略结算该访问时
  那么 按策略允许或拒绝，不因 PTC 路径一律禁止

Scenario: PTC 默认值全表
  Test: pending_req_fr_ptc_ptc
  假设 部署未覆盖 ptc-runtime-node 配置
  当 读取生效配置时
  那么 execution-mode 'per-call'、timeoutMs 120000、maxTimeoutMs 600000、maxOutputBytes 67108864、maxOldGenerationSizeMb 512、maxMessageBytes 134217728、maxPendingCalls 128、graceMs 3000

Scenario: 声明文本规范化
  Test: pending_req_fr_ptc_requirement
  假设 registry 含名称字典序交错、键含非裸标识符、JSDoc 含闭合序列的工具
  当 生成 TS 声明时
  那么 工具按 name 字典序、非裸键 JSON 引号、JSDoc 转义闭合序列、additionalProperties!==false 追加 Record 重载、敌意 schema 降级 unknown

Scenario: 换策略须先 release
  Test: pending_req_fr_ptc_release
  假设 persistent-in-memory 进程存活且请求切换沙箱策略
  当 策略切换结算时
  那么 先 release 旧进程再新建；进程存活期间同 owner 第二并发请求被拒

Scenario: dispatch-log 听众抛错 contained
  Test: pending_req_fr_ptc_dispatch_log_contained
  假设 dispatch-log waterfall 某听众抛错
  当 结算时
  那么 错误 contained 回退，不传播给程序或宿主

Scenario: model-direct 调用在管线前坍缩
  Test: pending_req_fr_ptc_model_direct
  假设 mode:ptc 下模型直接提交 run_code 以外的工具调用
  当 结算时
  那么 在策略管线前以 UNKNOWN_TOOL 加路由文本终结，绕过 pre-execute 与 guards

## Questions

- Source trace: doc/prd.md#9.4: run_code/PTC 完整规格（wire、能力面、失败分类、子调度、persistent-in-memory、node 缺失）, confidence: high（PRD 9.4 字段级规格）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
