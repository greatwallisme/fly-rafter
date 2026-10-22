# 合约覆盖度审计报告

| 项 | 值 |
|---|---|
| 审计对象 | doc/prd.md v1.0 ↔ doc/requirements-intake.md（20 块，100 条款） |
| 方法 | 逐节人工比对（PRD 每个功能点 → KLL 条款定位）+ 机械抽查（42 个 PRD 关键值 rg 命中计数） |
| 结论 | 发现 16 处缺口（6 大 10 小），全部为 PRD 忠实补录、无范围变化；修复后重跑门禁 |
| 日期 | 2026-10-21 |

## 一、逐节覆盖判定（PRD 节 → 合约条款）

| PRD 节 | 功能点 | 覆盖条款 | 判定 |
|---|---|---|---|
| 1.1/4.2 | 宿主 = 装配器 + 事件泵 + 入口分发；宿主对循环零感知 | （修复前无）→ CONFIG-7 | 修复 |
| 1.2 | `fr`、`~/.fly-rafter/`、家目录结构同构（sessions/storages/profiles） | CONFIG-6（路径）+ CONFIG-7（结构同构） | 修复 |
| 1.4/4.4 | 服务缝名与事件名与 dsh 逐字清单（16 缝 + 8 事件） | （修复前无）→ QUALITY-7 | 逐字清单缺失 → 修复 |
| 1.4/4.4 | Invoker 统一 `ctx.require`、禁 `ctx.get` | （修复前无）→ QUALITY-8 | 修复 |
| 1.4 原则 2 | 全插件（agent 循环也是插件） | AGENT-LOOP-1、CONFIG-7 | 已覆盖 |
| 1.5/19.1 | 排除清单 | SCOPE-1 | 已覆盖 |
| 4.1/4.3 | 插件三形态 + wasm 零能力精确含义 + WIT world dougong:kernel/host | （修复前无）→ CONFIG-8 | 修复 |
| 4.3/4.1 | 第三方工具 Invoker 通道接入模式；realm/isolate；internal/update 预留不暴露 | （修复前无）→ CONFIG-9 | 修复 |
| 4.5 | 26-crate workspace、依赖方向约束、dougong 依赖策略 | （修复前无）→ QUALITY-9 | 修复 |
| 5 | 品牌适配表 + !!js 子集 + 配置键 kebab | CONFIG-4、QUALITY-3、PROMPT-3、HEADLESS-2、SDK-4、ACP-1 | 已覆盖 |
| 6.1-6.3 | 分层模型、补丁行、schema 校验、dump 命令 | CONFIG-1..5 | 已覆盖 |
| 6.4 | 关键默认值（模型/权限/存储/超时/阈值） | CONFIG-6 | 已覆盖（会话标题默认值缺失 → 修复并入） |
| 7.1 | SessionEvent 全集 + 公共字段 + EpochHeader（reason 枚举） | SESSION-1 | EpochHeader 缺失 → 修复并入 |
| 7.1 | 消息角色五 + MessageSource + ContentBlock 七种 | （修复前无）→ SESSION-9 | 修复 |
| 7.2 | v4 文件、世代命名、非 v4 拒绝、SessionHeader 字段 | SESSION-2/8 | 写路径序列缺失 → 修复并入 SESSION-2 |
| 7.3 | surface/replace-op + system/message 节点流两路由 + 空渲染清空 | SESSION-3（replace-op）；两路由与空渲染 → SESSION-10 | 修复 |
| 7.4 | deriveMessages 冻结与确定性 | SESSION-4 | 已覆盖 |
| 7.5 | fork 与不可续用 | SESSION-5 | 已覆盖 |
| 7.6 | abort 补合成 tool/result | SESSION-6 | 已覆盖 |
| 7.1 | 未知必需事件拒绝重建 | SESSION-7 | 已覆盖 |
| 8.1/8.2 | 循环插件、七步控制流 | AGENT-LOOP-1/2 | 已覆盖（无适配器容忍中间件路由缺失 → 修复并入） |
| 8.3 | 失败恢复、provider_retry_policy | AGENT-LOOP-4 | 已覆盖 |
| 9.1-9.3 | 注册/schema/管线/调度 | TOOLS-1..4 | 已覆盖 |
| 9.4 | PTC 全链路 | PTC-1..9 | 已覆盖（TYPESCRIPT_FUNCTION_FLAVOR 缺失 → 修复并入 PTC-1） |
| 10 | 提示词组装、SECTION_ORDERS、身份句、脚本抽取 | PROMPT-1..5 | 已覆盖 |
| 11.1 | 审批契约（fail-closed 词表/归一化/事件/集成点） | APPROVAL-1..5 | 已覆盖 |
| 11.2 | 沙箱三预设、后端链、runtime-context 快照 | SANDBOX-1..4 | 已覆盖 |
| 12 | 压缩契约、剪枝、图片卸载、spill、/compact | COMPACTION-1..4 | 已覆盖 |
| 13.1/13.2 | LlmAdapter、StreamChunk 七种、三 provider、鉴权 | LLM-1..5 | 已覆盖 |
| 14 | MCP transport、命名空间、资源工具 | MCP-1..3 | 已覆盖 |
| 15 | 子代理 spawn/fork、ACP 桥、不做 cc/codex | SUBAGENT-1..3 | 已覆盖 |
| 16 | 斜杠命令 | COMMANDS-1..3 | 已覆盖 |
| 17 | 内置工具全集、catalog 冻结、ripgrep、ask fail-closed、无 plugin_market | BUILTIN-TOOLS-1..5 | 已覆盖（web provider 适配位缺失 → 修复并入） |
| 18.1 | headless 字段级 | HEADLESS-1..8 | 已覆盖 |
| 18.2 | TUI 字段级 | TUI-1..10 | 已覆盖（footer 贪心打包、max_scroll 每帧实测、翻页 ±5 行缺失 → 修复并入 TUI-3/4） |
| 18.3 | ACP 契约级 | ACP-1..6 | 已覆盖 |
| 18.4 | SDK 契约级 | SDK-1..5 | 已覆盖 |
| 18.5 | Web | 按 2B 裁决延后（doc/requirements-deferred-web.md） | 有意缺省 |
| 19.2/19.3 | 偏差治理、wire 层声明 | SCOPE-2/3 | 已覆盖 |
| 20 | M1 验收（逐事件同型 diff） | QUALITY-4 | 逐事件同型 diff 缺失 → 修复并入 |
| 21 | 质量门、工具链、命名、对照测试、文档即生成物 | QUALITY-1..6 | 已覆盖 |
| 22 | 热路径批量分发 + 基准进 M1 验收 | （修复前无）→ AGENT-LOOP-5 | 修复 |
| 22 | node/ripgrep/沙箱/漂移/提示词抄录对策 | PTC-9、BUILTIN-TOOLS-3、SANDBOX-3、SCOPE-4、PROMPT-5 | 已覆盖 |
| 23 | 15 条决策 | 全局映射各块 | 已覆盖 |

## 二、缺口清单（16 处）

| # | 严重度 | PRD 出处 | 缺口 | 修复 |
|---|---|---|---|---|
| G1 | 大 | 4.2 | 宿主形态三件事（装配器/事件泵/入口分发）与零感知约束无条款 | 新增 CONFIG-7 |
| G2 | 大 | 4.5 | workspace 布局与依赖方向约束无条款 | 新增 QUALITY-9 |
| G3 | 小 | 1.2 | FR_HOME 默认值与家目录结构同构未显式 | 并入 CONFIG-7 |
| G4 | 大 | 4.4 | 服务缝名（16）与事件名（8）逐字清单无条款 | 新增 QUALITY-7 |
| G5 | 大 | 4.4 | Invoker 统一 ctx.require 禁 ctx.get 无条款 | 新增 QUALITY-8 |
| G6 | 小 | 6.4 | 会话标题默认值全组参数未覆盖 | 并入 CONFIG-6 |
| G7 | 大 | 7.1 | 消息角色五 + MessageSource + ContentBlock 七种无条款 | 新增 SESSION-9 |
| G8 | 小 | 7.2 | 写路径编码→校验→独占发布序列未显式 | 并入 SESSION-2 |
| G9 | 大 | 7.3/8.2 | system/message 节点流两路由 + 空渲染清空无条款 | 新增 SESSION-10 |
| G10 | 小 | 8.2 | prepare_call 无适配器容忍中间件路由未显式 | 并入 AGENT-LOOP-2 |
| G11 | 小 | 9.4 | 语言 flavor 仅 TypeScript（TYPESCRIPT_FUNCTION_FLAVOR）未显式 | 并入 PTC-1 |
| G12 | 小 | 17 | web_search/web_fetch provider 适配位未覆盖 | 新增 BUILTIN-TOOLS-6 |
| G13 | 小 | 18.2 | footer 贪心打包、max_scroll 每帧实测、翻页 ±5 行未显式 | 并入 TUI-3/4 |
| G14 | 大 | 7.1 | EpochHeader 组成与 reason 枚举未显式 | 并入 SESSION-1 |
| G15 | 小 | 20 | 会话日志逐事件同型 diff（M1 验收 3）未显式 | 并入 QUALITY-4 |
| G16 | 小 | 22 | assistant-stream 热路径批量分发 + 基准进 M1 验收未覆盖 | 新增 AGENT-LOOP-5 |
| G17 | 大 | 4.3（用户追加质询） | 每行插件形态（builtin/dylib/wasm）可声明性与信任边界校验未显式（第一方全 builtin 由 PRD 4.3 全局固定，无需逐模块标注） | 并入 CONFIG-8（source kind 声明 + fr-config 信任校验，违规进 failures） |

## 三、修复执行记录

| 项 | 结果 |
|---|---|
| intake 修订 | 17 处（G1-G17）：CONFIG 6→9 条款、SESSION 8→10、AGENT-LOOP 4→5、BUILTIN-TOOLS 5→6、QUALITY 6→9；TUI-3/4、PTC-1、AGENT-LOOP-2、SESSION-1/2 并入增补；新增 2 个场景（宿主按子命令装配、空渲染清空 system 节点） |
| 重导入 | 20 块重导入 KLL（import 覆盖写、status 重置 proposed，按审计授权重新 transition accepted ×20） |
| 事故 1 | CONFIG 插入条款时锚点吞掉 `## Scenarios` 标题 → 导入后 scenario_count=0、draft 缺 CONFIG；回填标题修复 |
| 事故 2 | lint-knowledge 在草稿重生成前运行，误报 requirement-uncovered；调整时序后复跑通过 |
| 门禁复跑 | lint-knowledge --gate：0 errors（186 advisory）；graph --gate 通过；plan --gate 通过（20/20 work units ready）；20 份草稿 Quality 全部 100% |
| 机械验证 | 修复前 0 命中的关键值（MessageSource/ContentBlock/EpochHeader/TYPESCRIPT_FUNCTION_FLAVOR/事件泵/ctx.require/spill-store/会话标题/exa）修复后全部命中 |
| 条款总数 | 90 → 106（MUST 条款，实测计数；此前报告的 107 为算术错误） |

结论：PRD 第 1-22 节全部功能点均有对应合约条款覆盖（第 18.5 节 Web 按用户裁决 2B 延后，草稿保留于 doc/requirements-deferred-web.md）。

## 四、场景充分性与源码回灌（2026-10-21 第二轮）

用户标准升级：全部 20 份合约按"场景全过即 dsh 行为"裁决，不合格即重做；技术关键点必须回源码。

| 项 | 结果 |
|---|---|
| dsh 源码精读 | 4 个并行探查代理产出 doc/dsh-source-notes.md（127 行，全部 file:line 证据）：会话+循环 / 工具+PTC / LLM+提示词+审批 / 入口+装配 |
| PRD 勘误 | 12 处 PRD 文字与 dsh 源码不符，已按源码修正正文并记入 19.2 条 5（会话头 seedLength、tool/call 时序、绑定名规则、PTC wire 帧、程序 fs 访问、CONTEXT_ORDERS 行号、AGENTS.md 注入形态、image_request_target、SDK 未知方法错误码 -32603、空渲染语义、审批切换文案、独占发布细节）——待用户终审确认 |
| 条款回灌 | 106 → 119（SESSION+4、AGENT-LOOP+4、TOOLS+1、PTC+2、PROMPT+1、HEADLESS+1、SDK+1、ACP+1、CONFIG+1、LLM+1），每块 Source Trace 增加 doc/dsh-source-notes.md 指针 |
| 场景重写 | 48 → 261（每条款 Rule: REQ-FR-X-N 归属，clause-uncovered 归零）；含封闭集全枚举、异常路径、六个 dsh 产物 diff 锚点 |
| 门禁终态 | lint-knowledge --gate 0 errors（238 advisory）；graph/plan --gate 通过；20/20 work units ready；20 草稿全部真实 crate 边界（refine-drafts.py 纳入管线） |

### 20 份合约充分性裁决

| 合约 | 裁决 | 关键补强 |
|---|---|---|
| CONFIG | 补强后充分 | 装配树同型 diff、disabled/isolate/幂等、未记载默认值、信任校验 |
| SESSION | 补强后充分 | 发布协议、surface 硬约束、两级补偿、折叠编码、标题生成 |
| AGENT-LOOP | 补强后充分 | header 判定算法、缓存友好路由、inbox 语义、跨 turn 信号 |
| TOOLS | 补强后充分 | schema 白名单、guards 阈值、混合组屏障、参数快照 |
| PTC | 补强后充分 | wire 硬数值、8 类失败触发条件、子调用协议、escalation |
| PROMPT | 补强后充分 | orders 全表权威、组装确定性、AGENTS.md 注入形态、plan-mode 文案 |
| APPROVAL | 补强后充分 | 词表全枚举、asked/decided 配对、切换文案逐字 |
| SANDBOX | 补强后充分 | 平台后端链、观察策略对齐、fence |
| COMPACTION | 补强后充分 | 自动触发、summarizer 逐字、图片卸载 |
| LLM | 补强后充分 | SSE fixture 回放、装配结算、参数透传、代际绑定 |
| MCP | 补强后充分 | 双 transport、命名空间、资源三件 |
| SUBAGENT | 补强后充分 | fork 前缀派生、interrupt、仅 ACP 桥 |
| COMMANDS | 补强后充分 | 补全、三分发、headless 不解析 |
| BUILTIN-TOOLS | 补强后充分 | catalog diff 锚点、opt-in 语义、后台作业、todo 状态机 |
| HEADLESS | 补强后充分 | 全枚举事件流、截断降级梯、采纳四拒、语法错误 JSON 契约 |
| TUI | 补强后充分 | 键位全项、holdback、降级链、RAII、搬运出处 |
| ACP | 补强后充分 | 方法面全枚举、pin 机制、游标防伪、cwd 校验 |
| SDK | 补强后充分 | 方法与通知全枚举、错误码勘误、local 过滤、shutdown 序 |
| QUALITY | 补强后充分 | 四关、lints 对照、三 wire 锚点、逐事件同型 diff |
| SCOPE | 充分 | 排除枚举、偏差治理、wire 声明、锚定 |

## 五、场景规模终态（2026-10-21 第三轮）

用户裁决：261 条远不够，至少 1000 条；程序化生成允许但必须逐条核对、必须携带 dsh 技术细节。

| 层 | 数量 | 数据源 | 核对方式 |
|---|---|---|---|
| 手写行为场景 | 261 | PRD + doc/dsh-source-notes.md | 逐节人工比对 + 机械抽查 |
| 枚举场景 | 197 | 默认值 39 项/SECTION_ORDERS 32/CONTEXT_ORDERS 7/事件 19 类×3/chunk 7 型/失败 8 类/ACP·SDK 方法面/键位 26 | scripts/gen-scenarios.py 生成，逐条对回盘面依据（核对曾抓出 5 处笔记压缩失真并修正） |
| 目录场景 | 604 | doc/dsh-catalog-notes.md（tool-catalog 73 块中收录 50 工具 + config-catalog 相关 47 插件小节的结构化提取） | scripts/gen-catalog-scenarios.py 解析目录行生成，守恒断言（参数 103/行为 41/config 键 99 与解析数一一对应），每条场景内嵌目录行原文 |
| 合计 | **1062** | 三层全部可追溯 | clause-uncovered = 0；lint-knowledge --gate 0 errors（933 advisory）；20/20 work units ready |

新入库工具：scripts/gen-scenarios.py（枚举）、scripts/gen-catalog-scenarios.py（目录，含守恒断言）、scripts/refine-drafts.py（草稿精化）。
新增勘误：bash timeout 分层（bash-local schema 缺省 120000/上限 600000，base patch 覆盖 60000——两层并存均复现），已记 PRD 6.4 与 §17。

## 六、第二轮逐合约充分性审计（Phase 9，2026-10-22）

标准：每份合约自问"场景全过 ⇒ 开发出的插件与 dsh 相应插件近 1:1？"。方法：对照 doc/dsh-source-notes.md（本轮新增报告 5-8，4 个并行源码探查代理）逐条款核对；不充分者当场修补 intake 并走全管线落地（scripts/regen-pipeline.sh），每份独立门禁验证。场景 1062 → 1190。

| # | 合约 | 判定 | 修补要点 |
|---|------|------|---------|
| 1 | SESSION | 不充分→已修 | 源码矛盾 2：seedLength 不存在（isSeeded+磁盘六键 format.ts:96）；空渲染=清活跃尾部+head replace。新条款 15-17（投影/canonical/切点）+11 场景 |
| 2 | AGENT-LOOP | 不充分→已修 | attempt 三分支（interrupted:true）、user/message 仅 firstAttempt、状态机 step0/reject/maintenance、runtime-context admitted-once；条款 7/10/11 + 7 场景 |
| 3 | TOOLS | 不充分→已修 | 回源核实调度器：单调用屏障/池上限 10/lazy 重分类/concludeTurn 成功门控；guards 单调与拒绝后仍过 post-execute、双替换 TypeError、additionalProperties 显式布尔；条款 2/3/5 扩展 + 6 场景 |
| 4 | PTC | 不充分→已修 | 源码矛盾 2：fs 按沙箱策略（非禁止）、__proto__ 不拒绝（own key 注入）；8 类失败精确触发、硬数值条款 13（120000/600000/64MiB/512/128MiB/128/3000）、声明生成细则、persistent 释放门控、ptc 坍缩；+12 场景 |
| 5 | PROMPT | 不充分→已修 | CONTEXT_ORDERS 行号勘误（:170 非 :149）；AGENTS.md 注入=user 消息+system-reminder 帧（非 system section）；条款 7 全表 32+7 项数值内联（合约自足）；complete 唯一、interpolate:false |
| 6 | APPROVAL | 补强后充分 | 切换文案逐字、同值 no-op、abort 竞速 cancelled、effectivePolicy 优先级链 |
| 7 | SANDBOX | 不充分→已修 | 两层默认真值回源核实（插件 schema read-only vs 装配 !!js ?? workspace-write）；策略文本逐字模板（index.ts:44-47）；审批联动公式 |
| 8 | COMPACTION | 严重不充分→重写 | 4 条款→11 条款、5→19 场景：触发数学、摘要请求组成、目标三级回退、fail-closed 文案族、收缩门、保留配平、溢出恢复、持久括弧、剪枝细则、spill 布局与 footer、/compact 文案 |
| 9 | LLM | 不充分→已修 | image_request_target 补漏；新条款 7-11：持久化打包四 record、prepared 一次性、重试数值（5 次/500ms-10s/jitter 0.1）、deepseek 协议细则（/v1/MALFORMED_RESPONSE/message_stop 三重校验）、toolUpdate 能力差异 |
| 10 | MCP | 严重不充分→重写 | 3 条款→9 条款、6→23 场景：命名归一 sha256 后缀、生命周期/failOnStartupError、指令 32768、重连数学与耗尽、两段式同步回滚、调用错误文案族、内容投影逐字 |
| 11 | SUBAGENT | 严重不充分→重写 | 3 条款→10 条款、6→25 场景：spawn 生命周期事件、结果送达文案族、fork 种子规则、interrupt 四 UNAUTHORIZED、send_message 时序、list_agents 行格式、容量深度文案、preset 门控、委托策略 |
| 12 | COMMANDS | 严重不充分→重写 | 3 条款→7 条款、4→20 场景：解析正则/rawInput 保真、run/done 生命周期、五命令逐字文案族、/help 不存在负向、交互提交路径 |
| 13 | BUILTIN-TOOLS | 充分 | 目录层 408 场景守恒断言生成、行为行逐字内嵌 dsh tool-catalog 权威文本；框架条款齐备 |
| 14 | HEADLESS | 补强后充分 | trim 空参数、final 来源算法与提前唤醒抑制、usage 累加省略、tool_result append-only 投影 |
| 15 | TUI | 补强后充分 | 基线事实钉住：dsh TUI 包已上游删除（10bb9cbf4a），对齐对象=PRD 18.2 自有规格，dsh 键位不构成验收依据（负向场景） |
| 16 | ACP | 补强后充分 | resume 拒绝形态、list 过滤与同刻字节序、prompt 图片 invalidParams、request_permission 门控与 drainUpdates |
| 17 | SDK | 不充分→已修 | 未同步勘误修正：未知方法实得 -32603（-32601 仅 transport 无 handler）；initialize 参数校验、wait 未知 id、loader 就绪与 shutdown 退出序 |
| 18 | CONFIG | 不充分→已修 | 新条款 11/12：启动失败策略（required-id 审计同型）、整行替换非键级合并负向、env 缺省表达式映射；Open Question：dsh .env 分层是否纳入 fr 范围（待用户裁决） |
| 19 | QUALITY | 充分 | 元合约全覆盖，无修补 |
| 20 | SCOPE | 补强后充分 | 偏差清单与 PRD 19.2 现状同步（勘误+补遗条 5） |

统计：充分 3（BUILTIN-TOOLS/QUALITY/SCOPE 原充分 1+2）→ 实为充分 2、补强后充分 5、不充分重修 13（含 4 份严重不充分重写）。全轮每份修补独立过三门禁；终态 1190 场景、20/20 ready、0 errors。

### 待用户裁决事项（本轮新增）

1. TUI 基线：dsh 已删除 TUI 包（commit 10bb9cbf4a），fr TUI 对齐 PRD 18.2 自有规格——确认或改判。
2. dsh .env 分层（项目/home 两层 + bootstrap 专名拒绝 + 代理例外）不在 PRD 6.4——纳入 fr-config 范围与否（intake CONFIG 块 Open Questions 已记）。
3. PRD v1.1 的 12 处勘误+补遗（19.2 条 5）此前已列，本轮审计又在合约层同步了其中 3 处未落地的旧表述（SESSION seedLength/空渲染、SDK -32603、PROMPT :170）。

### 裁决结果（2026-10-22）

1. TUI 基线：用户确认——fr TUI 对齐 PRD 18.2 自有规格，dsh 已删包的键位不构成验收依据。
2. .env 分层：用户裁决纳入 fr-config——PRD 6.4 已增段（三层优先序/已设名不覆盖/bootstrap 专名拒绝含 FR_* 与 XDG_* 前缀/家目录代理四键例外）；CONFIG 条款 13 + 3 场景落地，Open Question 撤销。场景终态 1193。
3. PRD v1.1 勘误补遗及 3 处合约层同步：维持，无异议。

## 七、插件形态裁决返工（2026-10-22）

用户裁决（D1-D6 一次性裁决，回复"可以"通过）：废除 builtin 插件形态，插件仅 dylib/wasm 两形态；第一方全 dylib；fr-config 为宿主直链库非插件（D1A）；宿主保留内核+loader+fr-config+壳（D3A）；跨边界一律 Invoker JSON 字节、fr-wire 共享形状（D4A）；M1 六条验收文字不变（D5A）；合约全量扫描改写（D6A）。

- PRD 改写 11 处：1.4.2 原则、4.2 装配器、4.3 全节重写（两形态表+裁决依据+负向钉 builtin）、4.4 跨边界载荷形状、4.5 布局（fr-wire 入列、插件 crate 全 cdylib 仅依赖 dougong-abi）、6.2 产物解析规则（FR_PLUGIN_PATH → 安装库目录，lib<slug>.so / <slug>.wasm，未命中进 failures）、6.3 schema 真源宿主侧+双侧校验、8.1 循环 dylib 工件、20 M1 范围注记、22 风险对策、23 决策 16
- intake 改写：CONFIG-2（source 仅 dylib/wasm+解析规则+禁 builtin）、CONFIG-8（全节重写：两形态、第一方全 dylib、信任校验、禁 builtin）；场景 +3（第一方 wasm 拒绝改写、工件缺失失败、产物无 builtin）、改 3（Invoker 通道泛化、两形态装载、循环 fiber 工件归属）；AGENT-LOOP 场景 1 处
- 全管线复跑全绿：accepted×20、三门禁通过、场景 1193→1195

## 八、装载文件布局裁决（2026-10-22 Q1-Q5）

用户裁决：Q1A 家目录 patch = `<FR_HOME>/fr.patch.yml`（对齐 dsh home 根，PRD 6.1 勘误 + 19.2 条 7 + 23 决策 18）；Q2A profile 清单 = `<profile>/profile.yml`（bundles 仅引用宿主内置命名 bundle，未知名 skipped 警告非致命）；Q3A profile 模板自动初始化纳入 M1（1:1 dsh 首用 init，绝不覆盖既有文件；fr 模板 bundles=[] 为 Q2A+Q5B 推导——入口默认 bundle 恒在层 1）；Q4A 入口默认 bundle 为嵌入 YAML 常量、与用户 patch 同一解析路径；Q5B M1 默认 bundle 为最小 headless 集（14 行单 insert 组，config 值逐条对齐 PRD 6.4；工具级默认值随 Phase 16 fr-tools-builtin config schema 落地）。

- PRD 落地 5 处（6.1 勘误 + 装载文件语义段、19.2 去重 + 条 7、23 决策 18）；intake CONFIG-1 扩充 + 8 新场景；管线复跑全绿场景 1210。
- fr-config stack.rs 落地（装载/名字校验/清单/初始化/装配 + 11 tests），tests 26→37，四关全绿。
