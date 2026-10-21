spec: task
name: "headless 入口"
tags: [requirements, generated-draft, headless, entry, cli]
satisfies: [REQ-FR-HEADLESS]
---

## Intent

headless 是第一个交付入口（M1）：一次性任务、`--json` 事件流、审批 fail-closed、`--session-id` 续用。装配不挂任何 Host/HTTP/Web 运行时，禁用交互式凭证插件，无 UI answerer。

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

## Boundaries

### Allowed Changes
- crates/fr-headless/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 裸连字符从 stdin 读整段任务
  Test: pending_req_fr_headless_stdin
  假设 TASK 为裸 - 且 stdin 提供整段任务文本
  当 运行 fr headless 加该参数时
  那么 整段文本作为任务且不再等待更多输入

Scenario: 给了 TASK 不读 stdin
  Test: pending_req_fr_headless_task_stdin
  假设 命令行已给定 TASK
  当 运行时
  那么 stdin 被忽略且不阻塞

Scenario: 无 TASK 且 stdin 为 TTY 报 usage error
  Test: pending_req_fr_headless_task_stdin_tty_usage_error
  假设 终端交互运行且未给 TASK
  当 执行 fr headless 时
  那么 输出 usage error 且 exit 非 0

Scenario: 完整任务走通并产出 v4 会话
  Test: pending_req_fr_headless_v4
  假设 有效 provider 配置
  当 执行 fr headless 加任务时
  那么 最终答案到 stdout、产生 v4 会话文件且 exit 0

Scenario: 运行错误走 stderr 且 exit 1
  Test: pending_req_fr_headless_stderr_exit_1
  假设 provider 配置错误
  当 运行失败时
  那么 stderr 输出 fr 前缀错误行、exit 1 且 stdout 无答案文本

Scenario: reasoning 流到 stderr
  Test: pending_req_fr_headless_reasoning_stderr
  假设 模型输出含 reasoning delta
  当 默认模式运行时
  那么 reasoning 实时写入 stderr 且带 fr 前缀标题，stdout 不混入

Scenario: --json 首末事件定型
  Test: pending_req_fr_headless_json
  假设 --json 模式运行成功
  当 解析 stdout 事件流时
  那么 首事件 type 为 session，末事件 type 为 final 且答案无损

Scenario: 中间事件类型齐备
  Test: pending_req_fr_headless_requirement
  假设 任务触发工具调用
  当 解析 --json 流时
  那么 status、text、thinking、tool_call、tool_result 事件按序出现

Scenario: 丢弃的 attempt 不投影
  Test: pending_req_fr_headless_attempt
  假设 流错误后重试成功
  当 解析 --json 流时
  那么 text 与 thinking 仅来自已提交 assistant message，重试尝试文本不出现

Scenario: 值截断封顶
  Test: pending_req_fr_headless_requirement
  假设 某 tool_result 值超过 8 KiB
  当 输出该事件时
  那么 值被截断且标 truncated 为 true，单行不超过 32 KiB

Scenario: 运行错误输出 error 无 final
  Test: pending_req_fr_headless_error_final
  假设 --json 模式运行失败
  当 输出事件流时
  那么 含 error 事件且无 final 事件

Scenario: 需审批工具收到 unavailable
  Test: pending_req_fr_headless_unavailable
  假设 模型调用需审批工具且无 answerer
  当 工具进入审批时
  那么 模型收到 unavailable 拒绝结果

Scenario: danger-full-access 直接放行
  Test: pending_req_fr_headless_danger_full_access
  假设 FR_PERMISSION_MODE 为 danger-full-access
  当 工具执行时
  那么 策略为 never 且工具直接执行

Scenario: 续用已持久化会话
  Test: pending_req_fr_headless_requirement
  假设 存在一个已持久化的非 fork 非 子代理会话
  当 以 --session-id 指定该 id 运行时
  那么 接续其历史继续且新事件追加到原会话

Scenario: 不存在的 session id 失败
  Test: pending_req_fr_headless_session_id
  假设 --session-id 指向不存在的会话
  当 启动时
  那么 直接失败且不创建新会话

Scenario: 默认生成新会话 id
  Test: pending_req_fr_headless_id
  假设 未指定 --session-id
  当 启动时
  那么 创建形如 session 加 uuid 的新会话

Scenario: headless 装配无交互运行时
  Test: pending_req_fr_headless_headless
  假设 headless profile 装配完成
  当 检查装配产物时
  那么 无 Host/HTTP/Web 运行时插件、无交互式凭证插件、无 UI answerer

Scenario: --help 输出用法
  Test: pending_req_fr_headless_help
  当 运行 fr headless --help 时
  那么 输出用法说明且 exit 0

Scenario: 语法错误走 JSON 契约
  Test: pending_req_fr_headless_json
  假设 --json 模式传入非法参数组合
  当 解析失败时
  那么 stdout 输出 error 事件行且 exit 1

Scenario: key 级截断与降级梯
  Test: pending_req_fr_headless_key
  假设 事件含超长 key 与超大值
  当 boundJsonLine 处理时
  那么 key 与字符串值均封顶 8KiB，行超限依次降级为标量加 truncated 再到仅 type

Scenario: 采纳四拒与 live 拒绝
  Test: pending_req_fr_headless_live
  假设 --session-id 分别指向 preset 不匹配、子代理、cwd 不一致或已被 live Agent 占用的会话
  当 采纳时
  那么 一律拒绝且给出对应错误

Scenario: trim 空任务参数报错
  Test: pending_req_fr_headless_trim
  假设 任务参数为连续空白非空串
  当 启动时
  那么 报错且 exit 非 0；--session-id 空白串同理判空失败但原串保留于错误上下文

Scenario: final 来源与提前唤醒抑制
  Test: pending_req_fr_headless_final
  假设 会话日志含多段 assistant 文本且某子代理在收尾时提前唤醒主循环
  当 汇总输出时
  那么 final 取 durable log 最后一段非空 assistant text，唤醒以 seq 未变且无存活子判定抑制、继续等待

Scenario: usage 累加与省略
  Test: pending_req_fr_headless_usage
  假设 同一 step 跨两次 attempt 且其中一次无 usage 样本
  当 投影事件时
  那么 有效样本跨 attempt 累加；任一 attempt 无样本则整 step 省略 usage

Scenario: tool_result 仅 append 投影
  Test: pending_req_fr_headless_tool_result_append
  假设 某工具结果曾被压缩替换（surfaceOp 非 append）
  当 投影 --json 流时
  那么 原始结果不出现，仅 append 形态的 tool_result 投影

## Questions

- Source trace: doc/prd.md#18.1: headless 字段级规格（CLI、输出、--json、审批、多轮）, confidence: high（PRD 18.1 字段级）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
