spec: task
name: "斜杠命令框架"
tags: [requirements, generated-draft, commands, interaction]
satisfies: [REQ-FR-COMMANDS]
---

## Intent

交互入口需要斜杠命令。fly-rafter 提供 commands 服务与内置命令；headless 任务文本不解析命令。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- 解析 MUST：正则 `^\/([a-z][a-z0-9_-]*)(?=$|[\t\n\r ])`；rawInput = 精确余串含分隔空白、永不归一；无引号/转义规则（handler 自行拆分）；非命令返回未命中且不记日志；不以 / 开头（或裸 /）的输入按普通内容交给模型。
- 注册 MUST：名称 `^[a-z][a-z0-9_-]*$`、描述与 handler 与 hint 非空；同层重名 → `command "X" is already registered…`；scoped（每 agent）定义遮蔽 global；list() 名称序；注册表变更发 commands/change；补全元数据供交互入口消费。
- 执行语义 MUST：命令从不发给模型；handler 前 append command/run（载荷 {commandId,name,args?,source:{kind:'user'}}，recordInput:false 时省略 args）、结算后 command/done（载荷 {commandId,kind:'success'|'error',text?,sourceEventSeq?}）；二者 log-only 不进 transcript、不占 turn；commandId = `cmd-<8 位 uuid 片>-<单调序>`；handler 抛错/中止 → command/done kind:'error' 带渲染消息后重抛；附件给无附件命令 → `/<name> does not accept attachments`；无 store → `…attachments are unavailable because no attachment store is composed`。
- 内置命令 MUST 逐字对齐：/goal（语法 `[<objective>|clear|edit <objective>|pause|resume]`，空=显示 title/Status/Blocker/Objective/Rounds/Activation/Commands 块；已设/missing-goal/GoalError 各固定错误文案；附件在 create/edit 作 "Reference attachments for the goal objective." followup、非 create/edit 报错；clear 无 goal → "No goal to clear."）；/compact（无参强制 `Usage: /compact (no arguments)`；成功 `Compacted N history items (~M tokens).`、无历史 `No compactable history yet.`、取消 "Compaction cancelled."、错误码 busy/cancelled/changed/summary/commit/persistence 各固定文案）；/feedback（recordInput:false、空白 → "Feedback text is required. Usage: /feedback <text>"、成功 "Feedback recorded for session <id>\nAnonymous user: <id>."、append log-only feedback/record）；/permission（空参报当前 preset 与可用表、未知 → `unknown preset "X" (available: …)`、成功 `preset X`）；/plan（hint `[off|message]`，off 文案族 "Plan mode off." / "Leaving plan mode (applies from the next step)." / "Plan mode entry cancelled." / "Plan mode is already inactive."，on → "Plan mode on. Use /plan off to leave." 或 "Entering plan mode (applies from the next step).…"，非 off 消息与附件经 agent.steer 转发，off 带附件 → "Attachments cannot accompany /plan off."）。
- /help MUST NOT 注册（宿主无 help 命令；帮助卡由交互入口自行渲染）。
- headless MUST NOT 解析任何交互命令（任务文本非命令输入）。
- 交互入口提交路径 MUST：未命中的类命令行 → 错误 outcome 且保留草稿与附件；命中命令恒向入口报成功（结果经 command/run 与 done 广播渲染）；本地发 command/executed 事件仅提交方可见。

## Boundaries

### Allowed Changes
- crates/fr-commands/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 正则与 rawInput 保真
  Test: pending_req_fr_commands_rawinput
  假设 输入分别为 /goal 上下文含双空格、/goal、/1bad、裸 /
  当 parseCommand 结算时
  那么 首例 name 为 goal 且 rawInput 原样含双空格；/goal 命中空参；/1bad 与裸 / 不命中且不记日志、交模型普通输入

Scenario: 重名与遮蔽
  Test: pending_req_fr_commands_requirement
  假设 同层注册重复名、另一 agent 层注册同名命令
  当 注册与列举时
  那么 同层报 already registered；scoped 层遮蔽 global；list 名称序；变更发 commands/change

Scenario: run 与 done 配对
  Test: pending_req_fr_commands_run_done
  假设 一次成功命令与一次抛错命令
  当 结算时
  那么 成功者先后产生 command/run 与 kind success 的 command/done（log-only 无 turn）；抛错者产生 kind error 后错误继续传播；commandId 形如 cmd-<8位>-<序>

Scenario: 附件拒绝文案
  Test: pending_req_fr_commands_requirement
  假设 向无附件命令带附件、或在无 attachment store 部署带附件
  当 执行时
  那么 分别报 does not accept attachments 与 attachments are unavailable 逐字文案

Scenario: /goal 语法族
  Test: pending_req_fr_commands_goal
  假设 依次输入空参、clear（无 goal）、非法 edit、二次创建
  当 分发时
  那么 空参渲染完整状态块；clear 无 goal 回 No goal to clear.；非法 edit 报 usage；已设 goal 报固定错误；GoalError 回固定指引文案

Scenario: /goal 附件规则
  Test: pending_req_fr_commands_goal
  假设 create 带附件与非 create 子命令带附件
  当 分发时
  那么 前者以 Reference attachments for the goal objective. followup 入队，后者报错

Scenario: /compact 全分支
  Test: pending_req_fr_commands_compact
  假设 空闲无历史、空闲有历史、带参数、压缩被取消四种输入
  当 分发时
  那么 分别回 No compactable history yet.、Compacted N history items (~M tokens).、Usage: /compact (no arguments)、Compaction cancelled.

Scenario: /feedback 与 /permission
  Test: pending_req_fr_commands_feedback_permission
  假设 /feedback 空白与有文本、/permission 空参与未知名
  当 分发时
  那么 feedback 报必填文案或成功文案并落 log-only 记录；permission 报当前 preset 表或 unknown preset 文案，合法名切换成功回 preset X

Scenario: /plan 文案族
  Test: pending_req_fr_commands_plan
  假设 依次 /plan off（活动/非活动/待进入取消）、/plan 开启消息、/plan off 带附件
  当 分发时
  那么 off 文案族按态回正确一条；非 off 消息经 agent.steer 以用户消息转发并回开启文案；off 带附件报 Attachments cannot accompany /plan off.

Scenario: /help 未注册
  Test: pending_req_fr_commands_help
  当 输入 /help 时
  那么 宿主注册表无 help 命令，按未知命令路径处理

Scenario: 任务文本不触发命令分发
  Test: pending_req_fr_commands_requirement
  假设 任务文本以 /compact 开头
  当 headless 处理时
  那么 整段文本作为任务交给模型，不触发命令分发

Scenario: 未命中保草稿
  Test: pending_req_fr_commands_requirement
  假设 入口提交 /unknown cmd 带草稿与附件
  当 结算时
  那么 报错误 outcome 且草稿与附件保留不丢失；命中的命令恒报成功并以广播事件渲染结果

## Questions

- Source trace: doc/prd.md#16: commands 服务、内置命令 3 件、headless 不适用, confidence: high
- Replace pending test selectors with real test names before lifecycle verification.
