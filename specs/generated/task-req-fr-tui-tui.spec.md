spec: task
name: "TUI 入口"
tags: [requirements, generated-draft, tui, entry, ratatui]
satisfies: [REQ-FR-TUI]
---

## Intent

TUI 是主交互入口（M2）。为 ratatui 纯投影层：订阅会话事件 fold 进状态、只读渲染；band-stack 布局、单源键路由、modal 键盘词汇，范式承袭 shadow-tui。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- 技术栈 MUST：ratatui 0.29（unstable-rendered-line-info）+ crossterm 0.28 + pulldown-cmark 0.10 + unicode-width 0.2；fr-tui 为纯投影层：`Model::apply(event)` fold 进状态，`draw()` 只读渲染。
- 事件循环 MUST：spawn_blocking 轮询线程 → tokio mpsc → 主循环 `tokio::select!`（按键 rx / 事件 rx / 审批信号 / 问题信号 / 补全 debounce 120ms / 100ms 重绘 ticker / shutdown watch）；通信全走 channel。
- 布局 MUST 为 band-stack 单列无侧栏：header(1)/分隔线/内容时间线（弹性）/docked 卡区/activity 行/输入上分隔线/输入编辑框/输入下分隔线/排队提示行（有队列才占位）/help 行/footer 统计行；输入框随 wrap 实测行数增长（clamp MAX_INPUT_ROWS）；手动滚动暂停 auto-follow，滚回底部自动恢复；极矮终端（<8 行）按序降级（排队行/help/activity/输入框）；渲染异常降级为 notice 不 panic；docked 卡不遮时间线，仅审批/问题 modal 为 overlay；footer 贪心打包占多行；max_scroll 每帧实测。
- 键位表 MUST 全项生效（PRD 18.2 表）：Enter 发送；Ctrl+Enter/Alt+Enter/Ctrl+J/行尾反斜杠+Enter 插入换行；readline 编辑语义；上下方向翻 prompt 历史（带 draft 保护）；Ctrl+C running 中断、idle 双击退出（3 秒窗口）；Esc 中断或关 modal；PageUp/PageDown 翻页 ±5 行 / 鼠标滚轮滚动；Shift+Tab 循环 permission mode；Ctrl+O/T/R 展开工具卡/任务面板/子代理视图；空输入时 ? 帮助 overlay；bracketed paste 整段粘贴；kitty keyboard protocol 探测 CSI u。
- 审批卡 MUST：浮动 modal（tool:/kind: 元信息 + 命令全文手动 wrap、尾部必须可见、超长省略标注）；三选项 Yes(y)/Yes-and-dont-ask-this-session(a)/No(n)；y/a/n 单键直判，Up/Down+Enter 导航；选中行 `>` 标记反色；modal 独占键盘，Esc/Ctrl+C 永远可退出。
- ask_user_question 渲染 MUST：全屏对话框；数字 1-9 直选、Space 多选切换、Enter 激活、Tab/Shift+Tab 翻问题页、Esc 拒绝、常驻 Other 自由输入行；单问题单选选中即提交，多问题结束进 review 页。
- 渲染 MUST：markdown 经 pulldown-cmark → styled Lines；仅 4 语义色 + bold/italic/underline；表格宽度自适应收缩 + 单元格内 wrap；流式 holdback（未闭合表格整段扣住）；标题去 #；行内 code 着色；无语法高亮；Palette accent/warn/error/add/remove/user_bg/ok/dim + 按工具族着色；ColorMode 协商 None/16/256/TrueColor，尊重 NO_COLOR/TERM=dumb/tmux；diff 渲染 unified（add/remove 色）；工具卡三态（queued 静态 / running 闪烁 / 终态显示耗时）；交互输出全走 stderr，stdout 留给数据。
- 终端生命周期 MUST：TerminalGuard RAII（raw mode + alternate screen + bracketed paste + kitty 探测）；信号处理；SSH/tmux/CI 降级。
- 四搬运模块 MUST 保留出处头注释：自 ~/project/shadow/crates/shadow-tui 拷入 src/render/markdown.rs（1440 行）、src/render/diff.rs（289 行）、src/complete.rs（310 行 fuzzy 补全）、src/env/（约 1000 行：TermContext/TerminalGuard/kitty/信号）；其余按规格重写，禁止依赖 shadow-tui crate 本体。
- 斜杠与补全 MUST：`/` 命令补全与 `@` 文件补全，fuzzy 评分 + 120ms debounce；键位表逐项有 TestBackend 测试；tmux capture-pane 实机验证脚本入库。

## Boundaries

### Allowed Changes
- crates/fr-tui/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 事件 fold 进状态只读渲染
  Test: pending_req_fr_tui_fold
  假设 会话事件依次到达
  当 Model 逐事件 apply 且 draw 渲染时
  那么 渲染输出由事件序列唯一决定（TestBackend 断言）

Scenario: 多事件源单循环汇聚
  Test: pending_req_fr_tui_requirement
  假设 按键、会话事件、审批、问题、补全 debounce、重绘 ticker 与 shutdown watch 多源并发
  当 任一源触发时
  那么 全部经主循环 select 处理且通信全走 channel

Scenario: 极矮终端按序降级
  Test: pending_req_fr_tui_requirement
  假设 终端高度小于 8 行
  当 渲染时
  那么 按排队行、help、activity、输入框顺序降级且不 panic

Scenario: 手动滚动暂停与恢复 auto-follow
  Test: pending_req_fr_tui_auto_follow
  假设 用户滚动离开时间线底部后滚回
  当 新事件到达时
  那么 离开底部期间不跟随，回到底部后恢复自动跟随

Scenario: docked 卡不遮挡时间线
  Test: pending_req_fr_tui_docked
  当 工具卡以 docked 形式展示时
  那么 时间线完整可见，仅审批与问题 modal 为 overlay

Scenario: footer 贪心打包
  Test: pending_req_fr_tui_footer
  假设 统计项较多
  当 渲染 footer 时
  那么 统计项贪心打包占多行

Scenario: Ctrl+C 中断运行中会话
  Test: pending_req_fr_tui_ctrl_c
  假设 会话 running
  当 按下 Ctrl+C
  那么 发送中断且输入恢复可编辑

Scenario: idle 双击 Ctrl+C 退出
  Test: pending_req_fr_tui_idle_ctrl_c
  假设 空闲状态且 3 秒内第二次 Ctrl+C
  当 第二次按下时
  那么 退出 TUI

Scenario: 多行输入与整段粘贴
  Test: pending_req_fr_tui_requirement
  假设 输入框有焦点
  当 Ctrl+J、行尾反斜杠加 Enter 或 bracketed paste 时
  那么 换行插入或整段粘贴生效

Scenario: Shift+Tab 循环权限模式
  Test: pending_req_fr_tui_shift_tab
  当 连续按 Shift+Tab 时
  那么 按 read-only、workspace-write、danger-full-access 循环切换

Scenario: 上下方向翻 prompt 历史带 draft 保护
  Test: pending_req_fr_tui_prompt_draft
  假设 输入框有未发送草稿
  当 翻历史后再回到底部时
  那么 草稿内容恢复不丢失

Scenario: PageUp PageDown 与滚轮滚动
  Test: pending_req_fr_tui_pageup_pagedown
  当 按 PageUp/PageDown 或滚动滚轮时
  那么 时间线翻页或滚动，翻页步长 5 行

Scenario: y a n 单键直判
  Test: pending_req_fr_tui_y_a_n
  假设 审批 modal 打开
  当 按下 y、a 或 n
  那么 立即结算对应审批选项且无需 Enter

Scenario: Esc 永远可退出审批 modal
  Test: pending_req_fr_tui_esc_modal
  假设 审批 modal 打开
  当 按 Esc 或 Ctrl+C 时
  那么 modal 关闭且键盘焦点回归主循环

Scenario: 超长命令全文标注
  Test: pending_req_fr_tui_requirement
  假设 审批对象命令超过卡高
  当 渲染审批卡时
  那么 命令全文手动 wrap、尾部必须可见且超长部分带省略标注

Scenario: 数字直选与多选
  Test: pending_req_fr_tui_requirement
  假设 ask_user_question 对话框打开
  当 数字 1-9 直选、Space 多选切换、Enter 激活时
  那么 选择生效，Esc 拒绝，Tab 与 Shift+Tab 翻问题页，Other 自由输入行常驻

Scenario: 未闭合表格 holdback
  Test: pending_req_fr_tui_holdback
  假设 流式输出中出现未闭合表格
  当 渲染时间线时
  那么 该表格整段扣住，闭合后一并输出

Scenario: NO_COLOR 降级
  Test: pending_req_fr_tui_no_color
  假设 NO_COLOR 已设置
  当 渲染时
  那么 以无色彩模式输出

Scenario: 工具卡三态
  Test: pending_req_fr_tui_requirement
  假设 工具卡经历 queued、running 与终态
  当 各状态渲染时
  那么 分别呈现静态、闪烁与耗时三种形态

Scenario: TerminalGuard RAII 恢复
  Test: pending_req_fr_tui_terminalguard_raii
  假设 TUI 异常退出
  当 TerminalGuard drop 时
  那么 raw mode 与 alternate screen 恢复，bracketed paste 复位

Scenario: CI 环境降级
  Test: pending_req_fr_tui_ci
  假设 运行在 CI 或非交互终端
  当 TUI 启动时
  那么 按降级路径运行或明确报错，不破坏终端

Scenario: 四搬运模块保留出处头注释
  Test: pending_req_fr_tui_requirement
  当 检查 markdown.rs、diff.rs、complete.rs 与 env 目录文件头时
  那么 均保留 shadow-tui 出处注释且代码不依赖 shadow-tui crate 本体

Scenario: 命令与文件补全 debounce
  Test: pending_req_fr_tui_debounce
  假设 输入以 / 或 @ 开头
  当 补全 debounce 120ms 到期时
  那么 按 fuzzy 评分排序输出候选

Scenario: Enter 发送与 ? 帮助
  Test: pending_req_fr_tui_enter
  假设 输入框有内容或为空
  当 分别按 Enter 与 ? 时
  那么 有内容则发送，空输入时 ? 打开帮助 overlay

Scenario: Ctrl+O T R 面板
  Test: pending_req_fr_tui_ctrl_o_t_r
  当 分别按 Ctrl+O、Ctrl+T、Ctrl+R 时
  那么 展开最新工具卡、任务面板与子代理实时视图

Scenario: kitty 协议探测降级
  Test: pending_req_fr_tui_kitty
  假设 终端不支持 kitty keyboard protocol
  当 启动探测 CSI u 失败时
  那么 回退传统按键语义且 Ctrl+Enter 降级可用

Scenario: 不复刻 dsh pi-tui 交互形态
  Test: pending_req_fr_tui_dsh_pi_tui
  假设 以 dsh 最后版 TUI（packages/ui/tui，上游 commit 10bb9cbf4a 已删除）的键位与布局对照 fr TUI
  当 评审 TUI 行为时
  那么 对齐对象为 PRD 18.2 自有规格（ratatui band-stack、PRD 键位表、shadow-tui 范式）；dsh TUI 键位（如 Ctrl+L 重绘、Ctrl+O 三态循环）不构成 fr 验收依据，仅会话语义层（命令解析、审批、问题、steer/followup）与 dsh 对齐

Scenario: 键位 Enter
  Test: pending_req_fr_tui_enter
  假设 TUI 运行中
  当 按下 Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+Enter
  Test: pending_req_fr_tui_ctrl_enter
  假设 TUI 运行中
  当 按下 Ctrl+Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Alt+Enter
  Test: pending_req_fr_tui_alt_enter
  假设 TUI 运行中
  当 按下 Alt+Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+J
  Test: pending_req_fr_tui_ctrl_j
  假设 TUI 运行中
  当 按下 Ctrl+J
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 行尾反斜杠加 Enter
  Test: pending_req_fr_tui_enter
  假设 TUI 运行中
  当 按下 行尾反斜杠加 Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Backspace
  Test: pending_req_fr_tui_backspace
  假设 TUI 运行中
  当 按下 Backspace
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Delete
  Test: pending_req_fr_tui_delete
  假设 TUI 运行中
  当 按下 Delete
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 左方向
  Test: pending_req_fr_tui_requirement
  假设 TUI 运行中
  当 按下 左方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 右方向
  Test: pending_req_fr_tui_requirement
  假设 TUI 运行中
  当 按下 右方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Home
  Test: pending_req_fr_tui_home
  假设 TUI 运行中
  当 按下 Home
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 End
  Test: pending_req_fr_tui_end
  假设 TUI 运行中
  当 按下 End
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+A
  Test: pending_req_fr_tui_ctrl_a
  假设 TUI 运行中
  当 按下 Ctrl+A
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+E
  Test: pending_req_fr_tui_ctrl_e
  假设 TUI 运行中
  当 按下 Ctrl+E
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+U
  Test: pending_req_fr_tui_ctrl_u
  假设 TUI 运行中
  当 按下 Ctrl+U
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 上方向
  Test: pending_req_fr_tui_requirement
  假设 TUI 运行中
  当 按下 上方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 下方向
  Test: pending_req_fr_tui_requirement
  假设 TUI 运行中
  当 按下 下方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+C
  Test: pending_req_fr_tui_ctrl_c
  假设 TUI 运行中
  当 按下 Ctrl+C
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Esc
  Test: pending_req_fr_tui_esc
  假设 TUI 运行中
  当 按下 Esc
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 PageUp
  Test: pending_req_fr_tui_pageup
  假设 TUI 运行中
  当 按下 PageUp
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 PageDown
  Test: pending_req_fr_tui_pagedown
  假设 TUI 运行中
  当 按下 PageDown
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Shift+Tab
  Test: pending_req_fr_tui_shift_tab
  假设 TUI 运行中
  当 按下 Shift+Tab
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+O
  Test: pending_req_fr_tui_ctrl_o
  假设 TUI 运行中
  当 按下 Ctrl+O
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+T
  Test: pending_req_fr_tui_ctrl_t
  假设 TUI 运行中
  当 按下 Ctrl+T
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 Ctrl+R
  Test: pending_req_fr_tui_ctrl_r
  假设 TUI 运行中
  当 按下 Ctrl+R
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 ?
  Test: pending_req_fr_tui_requirement
  假设 TUI 运行中
  当 按下 ?
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

Scenario: 键位 滚轮
  Test: pending_req_fr_tui_requirement
  假设 TUI 运行中
  当 按下 滚轮
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

## Questions

- Source trace: doc/prd.md#18.2: TUI 字段级规格（技术栈、布局、键位表、审批卡、问题对话框、渲染、终端生命周期、搬运清单、补全）, confidence: high（PRD 18.2 字段级）
- Replace pending test selectors with real test names before lifecycle verification.
