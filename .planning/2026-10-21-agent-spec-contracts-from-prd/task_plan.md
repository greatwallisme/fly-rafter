# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
Phase 16 批 0 **16a 真源深读完成**（shell 缝/shell-env/bash 双执行体/tool-bash 三件/subprocess 缝/subprocess-local(index)/jobs 五件/pwsh 四件，~4.5k 行 TS，详见 findings「16a 域深读」七节）。**下一动作=批 0 分叉四项呈报用户裁决**（findings「16a 批 0 分叉呈报」节：①DSH_*/FR_ 前缀 ②托管域分级 ③pwsh Linux 地位 ④SESSION_ID 来源）→裁决后批 1（fr-shell 实现：三服务 provider+注册面+钉测）。
Phase 16（fr-tools-builtin + fr-shell）——批 0 真源全读待启（Phase 14 已收口：批 0 裁决/批 1 fr-tools+guard/批 2a approval+presets+projections/批 2b sandbox/批 3 绑定+七步流 4-7）

## Phases

### Phase 1: 环境与切分设计
- [x] agent-spec CLI 前置检查（1.4.0 已安装）
- [x] 读 patterns.md / commands.md 参考
- [x] 读 doc/prd.md 全文，确定切分粒度：按子系统 21 块（PRD 第 5-21 节映射）
- **Status:** complete

### Phase 2: 起草候选需求块
- [x] 写 doc/requirements-intake.md（21 个 `<!-- agent-spec:requirement -->` 标记块，955 行）
- [x] 每块含 Problem / Requirements(MUST) / Scenarios / Dependencies / Source Trace / Open Questions
- [x] 结构校验：21 起始 + 21 结束标记、21×6 必需节、依赖图无环
- **Status:** complete

### Phase 3: 用户确认候选块
- [x] 用户裁决 1A 2B（2026-10-21）：粒度确认；WEB 块移出本期导入（草稿存 doc/requirements-deferred-web.md）
- **Status:** complete

### Phase 4: 导入 KLL + 门禁
- [x] `agent-spec init --workspace`（knowledge/ 五目录 + canon + config）
- [x] `requirements import`（20 块全部导入，status: proposed）
- [x] `lint-knowledge --gate` 通过（0 errors；167 条 advisory 提示已清单化）
- [x] `requirements graph --gate` 通过；20 块全部 `transition --to accepted`（依据本轮用户确认）
- [x] `requirements plan --gate` 通过（修复：删除生成器写入的悬空 `depends: [REQ-*]` 行——spec 级依赖应为 spec 名，需求级依赖真源在 KLL）
- **Status:** complete

### Phase 5: 反向访谈与答案回写
- [x] `requirements questions`：31 条全部 blocking:false（weak-then/single-statement 等 advisory 诊断），无阻断问题，访谈折叠；清单存 `.agent-spec/questions-2026-10-21.json`
- **Status:** complete

### Phase 6: 生成任务合约
- [x] `work-units` + `draft-specs`：20 份草稿落盘 `specs/generated/`，Quality 全部 100%
- [x] test-obligations 清单存 `.agent-spec/test_obligations.json`
- [ ] 里程碑启动时的人工精化：绑定真实测试选择器、补 stderr/输出模式场景、Rule 分组、真实 Boundaries（草稿态按设计预期 lifecycle 不通过）
- **Status:** complete（草稿态交付；精化移至 M1 启动）

### Phase 7: 依赖图与验证基线
- [x] `trace REQ-FR-HEADLESS` 基线：liveness=Unproven、场景 Skip（pending 选择器，无代码前预期态）
- [x] worktrees 清单跳过（仓库尚无提交、无 base 分支可排程，记录于 findings）
- **Status:** complete

### Phase 8: 覆盖度审计与修复（用户指令：检查合约是否全覆盖功能点细节）
- [x] 逐节人工比对 + 42 项机械抽查 → doc/coverage-audit.md（16 缺口）
- [x] 用户追加质询（插件形态 dylib/wasm 标注）→ G17 条款化（第一方全 builtin 无需逐模块标注；source kind 声明 + 信任校验入 CONFIG-8）
- [x] 17 处修复写入 intake → 重导入 → transition accepted ×20 → 门禁全绿（0 errors、20/20 ready、20 草稿 Quality 100%）
- [x] 事故记录：CONFIG 锚点吞 `## Scenarios` 标题致 draft 缺失；lint 时序误报——均已修复并复跑验证
- **Status:** complete

### Phase 9: 逐合约充分性审计（用户指令：场景全过是否即近 1:1；每审完一份立刻修改）
- [x] 弱域真源补齐：4 个并行探查代理（TUI/命令+子代理/压缩+MCP/装配+其余插件）→ dsh-source-notes.md 报告 5-8（含 dsh TUI 包已上游删除的基线事实）
- [x] 20 份合约逐份审计 + 即时修补：充分 2、补强后充分 5、重修 13（COMPACTION/MCP/SUBAGENT/COMMANDS 四份严重不充分重写）
- [x] 3 处源码矛盾修正（seedLength、空渲染、SDK -32603）+ PTC 2 处（fs 沙箱/__proto__）+ PROMPT 行号
- [x] scripts/regen-pipeline.sh 入库；每份修补独立过三门禁；场景 1062→1190
- [x] doc/coverage-audit.md 第六节裁决表 20 行裁决 + 3 项待裁决事项落盘
- **Status:** complete

### Phase 10: 裁决落地与首次提交
- [x] 三项裁决落地（TUI 基线、.env 分层 CONFIG-13、勘误维持），全管线复跑全绿（1193 场景）
- [x] root commit `7bf9493`（70 文件，26045 行），工作树干净
- **Status:** complete

## M1（headless）实现阶段 — PRD 第 20 节

范围：workspace 骨架与质量门；fr-config 分层编译（含 .env 分层与 dump-config/dump-config-schema）；fr-session v4 全量；fr-agent-loop；fr-tools 管线 + 第 17 节全部工具（subagent-acp 桥除外）；fr-approval/fr-sandbox；fr-compaction；fr-system-prompt；三 provider；fr-mcp；fr-ptc；进程内子代理 spawn/fork；fr-shell（bash seam + 作业管理，工具域依赖）；fr-headless + fr 宿主。验收：1. `fr headless "任务"` 完整 turn 周期产出 v4 会话文件；2. `--json` 事件流过 schema 校验；3. 与 dsh 同任务会话日志逐事件同型；4. 审批 fail-closed / --session-id 续用 / abort 补偿集成测试；5. dump-config 系可用；6. 质量门四关全绿。

### Phase 11: workspace 骨架与质量门基线
- [x] root Cargo.toml（resolver 3、edition 2024、workspace.lints 对齐 dougong、workspace deps）
- [x] rust-toolchain.toml（1.99.1，与 dougong 补丁级对齐）
- [x] fr-config 骨架（首批 crate，含最少实现过四关）
- **Status:** complete（commit `0d3e87e` 起；回填勾选 2026-10-22）

### Phase 12: fr-config 分层编译
- [x] 2026-10-22 Q1-Q3 裁决：合并语义对齐 dsh applyEntryPatches（按键覆盖/warn-skip/首插序）；insert 根级；!!js 子集扩充（env ?? 链/相等/平台三元/裸 env/字面量/profile-context 布尔）——PRD 6.1/6.2/5/19.2/23 与 intake CONFIG-1/4/11/12 已勘误，管线全绿 1202 场景
- [x] 补丁行模型 + 合并器 compose（dsh 语义）+ 产物解析 + 信任校验 + 编译产出 doc+failures（禁 builtin）
- [x] cond.rs 受限表达式求值器（递归下降；!!js→!js 行锚定归一化绕 libyaml tag 丢失）
- [x] 分层文件装载栈（项完成于 2026-10-22，Q1-Q5 裁决落地：stack.rs 11 tests，四关全绿）
- [x] ConfigSchema 声明框架（2026-10-22 Q7-Q10 裁决落地：fr-schema 独立 crate 描述树单一真源 + validate/to_json_schema 双派生 + registry lookup absent 语义；fr-config compile_to_loader_doc 挂接 schema_of 校验槽，2 钉测；四关全绿 42+15 tests）
- [x] .env 分层（2026-10-22 Q11-Q14 裁决落地：49 名全表 + 四前缀、失败语义五点、materialize+快照双轨、Node util.parseEnv 方言自实现 + 官方 fixture 钉测、resolve_home；env.rs 19 tests，四关全绿）
- [x] dump-config / dump-config-schema 数据面 + CLI 流面（CLI 随 Q30B 宿主切片落地于 crates/fr，2026-10-23）
- [x] 对照 REQ-FR-CONFIG 场景逐条落测试（已绑 55，其余 259 pending 随后续 crate 切片递增）
- **Status:** complete

### Phase 13: fr-session v4 全量
- [x] SessionEvent 全集（判别联合、dsh types.ts 逐字字段、五扩展 log-only）——fr-wire 切片 A/B 落地
- [x] 持久化物理不变量（wx+0600+fsync → link+unlink 发布 → 目录 fsync → append truncate 回滚 → zstd 撕裂尾恢复 → 写租约 POSIX 侧）与世代/版本校验（切片 E）
- [x] 持久化运行时（切片 F，Q42A'：handle actor worker/tracker/live 200ms 路由/flushAll/stat/list/create/open 流/有界并行 list/版本门/legacy 拒——19 钉测含 10 并发句柄与大会话库极端测）
- [x] surface/replace-op 硬约束全集（五类入表面、双闭区间、sourceEventSeqs 校验、折叠编码、双计数）——切片 C 落地
- [x] deriveMessages 投影（不可变请求投影）+ canonical request/header（空 tools 省略、adapterDefaults 条件保留、顺序敏感相等比较）——切片 D 落地
- [x] fork（sourceEventSeqs 前缀派生；build_fork_seed + forked closers 已落）+ store 侧 restore 原语（切片 G）；host 层 resume 四拒门禁在 Phase 18（PRD:323）
- [x] SessionStore 内存层（prepare/enter/announce/fork/restore/flush 检查点/四 feed/拒绝码族，切片 G 落地）
- [x] 切片 H：plugin 服务面（create/append/flush/fork/resume/restore/drain 七方法号 + fr-wire 载荷）+ tracker 装配（store 四监听 ↔ persist 路由面）+ Q43-A drain 面（per-session cursor 镜像，2026-10-24 落地，299 tests 四关全绿）；dylib 壳接线随 Q44-B fabric INIT/Phase 18 装配器
- [ ] 对照 REQ-FR-SESSION 场景逐条落测试（75 passed / 27 skipped 维持；bind-session-tests.py 绑定基础设施属切片 H）
- Foundation Note: 本 crate 是管线咽喉：fr-agent-loop/fr-headless 依赖其 deriveMessages/surface 语义，语义字段逐字对齐 dsh types.ts；日志内 turn 编号 1 起（findings 切片 F 节登记）
- **Status:** in_progress

### Phase 13b: fr-llm seam + 三 provider
- [x] fr-llm：LlmAdapter trait（stream 必实现 + 7 默认方法）、GenerateOptions、StreamChunk 七种、流装配结算（block-end 上 delta 忽略、重复 close 忽略、delta-only 兜底、缺 id/name 兜底 call-<index>、max-tokens 过滤、ReplayEnvelope 一致性）、prepare_call 代际绑定、toolUpdate 能力差异表（批 A/B 落地，36ef3c1 + 11ee0a9）
- [x] provider_retry_policy 结构（normal/always、maxRetries 5、retryableCodes 默认 5 类、backoff 500/10000/jitter 0.1 对称抖动；执行方 llm-retry 挂 agent 失败步扩展点，LLM 核心不自执行）（批 A 落地 36ef3c1，Q60A 精化 40f1801）
- [x] fr-llm-deepseek：Anthropic Messages 协议（切片 3，ed3cf97）+ 网络执行面 13 模块 + fr-timeout 新 crate（切片 4，93bf613；96 tests，三 crate 四门绿 + 门禁序列全跑毕）
- [x] fr-llm-openai（切片 5 收口 5671260：24 模块 103 tests，三场景绑定，PRD 决策 44 + 19.2 条 22/23）
- [x] fr-llm-anthropic（切片 6 收口 2026-10-27：19 模块 83 tests，两场景绑定，PRD 决策 45 + D15-a stealth 移植 + A2 勘误 in-history 结构性不可达三 provider 基线折叠）
- [x] 对照 REQ-FR-LLM 场景逐条落测试（provider 合约 lifecycle 40/40 passed / 0 skipped / 0 failed 全闭环；stamp --dry-run Spec-Passing true）
- **Status:** complete

### Phase 14: fr-tools + fr-approval + fr-sandbox（Q91-A 先行；批结构 Q94-A：批 0 裁决/真源→批 1 fr-tools→批 2 approval+sandbox→批 3 绑定）
- [x] fr-tools：ToolRegistry（register：名称/JSON Schema/executionMode/presentation/section order 1000-3000）、MCP raw schema 动态注册；**监听注册面（Q101′A：pre-execute/execute/post-execute 分 stage 回调端点，注册序=装配行序=触发序）**——批 1e 落
- [x] guard 两插件 crate（Q100′A）：fr-guard-repeat-reminder（阈值 [3,5,8]/预览 500/用户消息重置链经 agent/user-admitted）/fr-guard-timeout（TOOL_TIMEOUT 施加）——批 1e 落
- [x] 执行管线五段（createExecution 冻结 → pre-execute waterfall（ask 交 ApprovalService）→ guards 单调串 → tools/execute waterfall → 工具体 → post-execute）；ptc 坍缩调用策略前置终结；post-execute 双替换 TypeError/失败结果禁 value 替换/finalize 快照冻结；成功结果过 output schema 校验——批 1e 落
- [x] 调度（isConcurrencySafe 严格 true 才 parallel；exclusive 组=单调用屏障；池上限 max-parallel-tool-calls 默认 10；lazy 重分类；concludeTurn 成功门控）——实现在 fr-agent-loop/driver.rs（Q-A 裁决边界扩域），批 3 落 7 件调度钉测
- [x] fr-approval：ApprovalPolicy ask/never、request() fail-closed 'unavailable' 归一化、approval/policy 事件、'never' dispatch 前自判（审计对仍产生=Q-C 勘误）、ask id UUID v4（Q-D）、竞速载体=工具管线（Q-E）、切换文案逐字——批 2a+批 3 落
- [x] fr-permission-presets（Q104′A 独立 crate）：双 knob 捆绑表+catalog+set/current/derive+pinInitialPermission 播种+/permission 命令行为面（注册随 Phase 18 fr-commands 翻转）——批 2a 波 2 落（2026-10-29，Q110′A′ 自持折叠）
- [x] fr-session 增 'session-projections' 服务面（Q105′A：register/stateOf；sandboxMode/permissions 投影消费者）——批 2a 波 2 落（1000+ 号段+root 载体冷读继电）
- [x] fr-sandbox：三预设表、FR_PERMISSION_MODE 覆盖、后端链 bubblewrap→Landlock/Seatbelt/ACL、sandbox-unavailable fail-closed——批 2b 落
- [x] 对照 REQ-FR-TOOLS/APPROVAL/SANDBOX 场景逐条落测试——批 3 落（43/43 真实绑定，lifecycle 19+15+9 全过零跳过，liveness 三需求 Honored）
- **Status:** complete（2026-10-29 批 3 收口）

### Phase 15: fr-system-prompt + fr-agent-loop（切片 7/8，已闭环）
- [x] 提示词抽取脚本（scripts/extract-prompts，从 dsh 源文件生成常量，禁手抄）——批 1a 落（FR_DSH_SOURCE 必需+--check 钉测+生成管道内嵌 rustfmt）
- [x] fr-system-prompt 纯层与域（有序 section/context 注册、SECTION_ORDERS/CONTEXT_ORDERS 全表生成、{{var}} 校验四文案逐字、tool-order 五态、complete 至多一个+waterfall 恢复、AGENTS.md 域五模块全量）——批 1b/1c 落；服务面+壳——批 2a 落
- [x] loop 桥（批 2b：prompt_client+InstructionsBridge+loop_inbox 增面+touched 收集+往返钉测）——2026-10-28 落，crate 82/workspace 856 四门绿
- [x] fr-agent-loop 全量（切片 7 落：AGENT-LOOP 23 真实+3 跨切片 skip）
- [x] 对照 REQ-FR-PROMPT/AGENT-LOOP 场景逐条落测试（PROMPT 46 真实+1 skip=working-directory Phase 18，2026-10-28 批 3）
- **Status:** complete

### Phase 16: fr-tools-builtin + fr-shell（Q92-A 单合同双 crate：builtin-tools spec Boundaries 增 crates/fr-shell/**；Q93-A fr-tools-builtin provide 全部域服务（fs/web/goals/schedule/sessionQuery/skills/lsp/workingDirectory/userQuestions/cordisInspect/pluginManager/shellEnv/terminals），fr-shell 仅 provide shell/subprocess/jobs；Q94-A 拆 16a shell 组/16b 文件组 7 件/16c 任务状态/16d 交互+会话查询/16e PTY+persistent/16f web+skill+lsp+目录+内省；Q95-A catalog 脚本 Q89-A 判例同款；Q96-A run_code+subagent 族绑跨切片 skip；Q97-A 每工具一件全 schema 钉测）
- [ ] fr-shell：bash 执行 seam、子进程/作业管理（job_kill/list/output）、沙箱策略执行体对接
- [ ] 第 17 节工具：shell 组（bash/pwsh/persistent）、文件组 7 件、web 2 件、todo/goal/jobs/schedule、交互 2 件（headless fail-closed）、会话查询 5 件、PTY 6 件（opt-in）、skill 2 件、lsp、working_directory、内省 3 件
- [ ] 每工具参数 schema 对照 dsh tool-catalog.md 逐字冻结
- [ ] 对照 REQ-FR-BUILTIN-TOOLS 场景逐条落测试
- **Status:** modules pending

### Phase 17: fr-compaction + fr-mcp + fr-ptc
- [ ] fr-compaction：剪枝 8192/4096/1024、压缩契约与自动触发、图片卸载、spill max-inline-tokens 12500、replace-op 落地
- [ ] fr-mcp：stdio + Streamable HTTP、命名空间、raw schema 动态注册、资源工具 3 件
- [ ] fr-ptc：node 子进程 strip-only、wire 4B BE 长度前缀、8 类崩溃分类、绑定名校验并集、声明生成、子调度 submission-ordered 池 10、approveEscalation、persistent-in-memory opt-in
- [ ] 对照 REQ-FR-COMPACTION/MCP/PTC 场景配平
- **Status:** modules pending

### Phase 18: fr-subagent + fr-headless + fr 宿主
- subagent spawn/fork 子 realm、配套 5 工具；headless CLI/输出/截断梯/--session-id 四拒门禁；fr 装配器+事件泵+入口分发（dump-config 系 CLI 已随 Q30B 切片前置于 Phase 12 落地）；M1 验收六条逐条验证
- **Status:** pending

### Phase 19: M1 验收收尾
- [ ] 六条验收标准逐条验证（headless 走通/--json schema 校验/与 dsh 逐事件同型 diff/三集成测试/dump-config 系/四关全绿）
- [ ] 生成式文档（tool-catalog/config-catalog 等价物）与 dsh 同名目录 diff 评审
- [ ] 里程碑精化：绑定真实测试选择器替换 pending_*（按 crate 逐个）、lifecycle 从 Skip 转真实验证
- **Status:** pending

## Decisions Made
| Decision | Rationale | Rationale |
|----------|-----------|
| 粒度 = 每子系统一个 REQ 块（21 块） | 每块对应 PRD 独立可验证行为域；工作单元阶段再按里程碑归组 |
| KLL 场景 DSL 用中文（场景/假设/当/那么） | 解析器双语支持（patterns.md）；项目文档语言为中文 |
| 标记属性仅 id/title/tags/source | commands.md 只记载这四个属性；confidence 放 Source Trace 条目，避免未记载属性引发导入风险 |
| 候选块独立落盘 doc/requirements-intake.md | import 只读显式标记块、不解释原始散文；PRD 保持纯净 |
| Q91-A（2026-10-28）次序勘误：Phase 14 先行，随后 Phase 16 | 五重证据证实 Phase 14 未做（crate/members/spec pending/task_plan/findings 零记录）而 CLAUDE.md 虚指已落；Phase 16 硬依赖 14（注册表/审批/沙箱是 builtin 工具装配前提）；CLAUDE.md 已同批勘误 |
| Q92-A fr-shell 合同承载：扩 builtin-tools spec（Boundaries 增 crates/fr-shell/** + Decisions 增 shell seam 条款） | 单合同双 crate，Phase 16 一体闭环；fr-shell 无独立 KLL 需求块 |
| Q93-A 域服务 provider 归属：fr-tools-builtin provide 全部域服务（fs/web/goals/schedule/sessionQuery/skills/lsp/workingDirectory/userQuestions/cordisInspect/pluginManager/shellEnv/terminals）+全部工具；fr-shell 仅 provide shell/subprocess/jobs | PRD 4.5 无域服务 crate；plugin-first 达标（独立插件 provide+require 消费）；terminals 随工具域归 fr-tools-builtin |
| Q94-A 切片拆分：Phase 14 单切片四批；Phase 16 拆 16a shell 组/16b 文件组/16c 任务状态/16d 交互+会话查询/16e PTY+persistent/16f web+skill+lsp+目录+内省 | 真源合计 ~50k 行，单切片不可收口 |
| Q95-A catalog 生成脚本：Q89-A 判例同款（python3+FR_DSH_SOURCE 必需+--check 钉测缺席即 fail）；抽取源=docs/tool-catalog.md；产物=fr-tools-builtin schema 常量表+装配 dump 与冻结 catalog diff 为空钉测 | spec 决策钦定 catalog 为权威；判例降低分叉 |
| Q96-A run_code+subagent 族五工具：本切片不实现，场景绑跨切片 skip（AGENT-LOOP 判例同形） | 实现归 fr-ptc（Phase 17）/fr-subagent（Phase 18），各自有独立 spec |
| Q97-A schema 场景绑定：每工具一件全 schema 钉测（断言该工具完整参数表）覆盖其全部形状+约束场景，绑定审计逐映射 | Q90-A 同构；~120 逐参数场景收敛为 ~40 件钉测 |
| Q98-B（2026-10-28）permission-presets 服务面 Phase 14 全落（表+命令行为面+投影+播种；Auto 动态钩子不落） | 用户裁决；命令注册随 Phase 18 fr-commands 所有权翻转落地 |
| Q99-B（2026-10-28）win32 ACL 执行体本切片全量移植（~2000 行 cfg(windows)+交叉编译门） | 用户裁决；19.2 条 15 原则的显式例外，运行时验证空缺如实档载 |
| Q100′A（2026-10-28）guard 两 crate：fr-guard-repeat-reminder+fr-guard-timeout（1:1 dsh 两包两装配行） | 一切皆插件（用户原则重申加严：权重禁实现便利，只以使用/维护/升级为准） |
| Q101′A（2026-10-28）tools 监听注册面：分 stage+显式序，注册序=装配行序=触发序 | 决策型瀑布跨插件的唯一 ABI 可行路径（dylib 无出站 dispatch）；通用策略插件入口 |
| Q102′A（2026-10-28）agent/user-admitted 通知事件（agent-loop emit+guard 原生 listen） | 通知型走 dougong 原生事件面；dsh agent/pre-step 用户消息观测语义的 fr 载体 |
| Q103′A（2026-10-28）PRD 4.5 增三行+TOOLS spec 边界扩两 guard crate | Q92-A 判例同形；守卫场景绑 guard crate 真实测试 |
| Q104′A（2026-10-28）fr-permission-presets 独立 crate（撤销 Q98-B 的 fr-approval 内挂记录） | dsh 独立包 1:1；审批与预设两策略独立演进 |
| Q105′A（2026-10-28）'session-projections' 服务现在立缝（fr-session provide，register/stateOf） | dsh 同域独立服务；Phase 18 ACP 零重构接入 |
| Q110′A′（2026-10-29）投影折叠所有权=注册者自持折叠（自读=read 尾拉+本地折叠；第三方=fr-session 继电纯折叠端点；注册表 M1 无 cell） | dougong 同步 wire 锁序三路证明注册表侧折叠不可服务自读；单实现零漂移（B′ 双份折叠弃） |

## Errors Encountered
| Error | Resolution |
|-------|------------|
| 单次 Write 生成 955 行长文档时输出退化：SANDBOX 块标记错写为 agent-spec:block、后半 13 块缺失、尾部噪声、多处乱码 | 三振协议换方法：整体重写为分批写入（Write 前 3 块 + 6 次 heredoc 各 3 块），每批后 rg 校验标记数与乱码扫描；另修复 3 处乱码（Sur一概/prd页）与 WEB 块重复 Dependencies 节 |
| 首次 Write 路径笔误 /home/greatclosing（不存在） | 改用正确路径 /home/greatwallisme/project/fly-rafter/doc/requirements-intake.md |
| lint-knowledge --gate 首跑失败：唯一 Error 为 specs/ 目录不存在 | mkdir -p specs 后复跑通过（该目录随即被 draft-specs 填充） |
| plan --gate 失败：35 条 dangling-spec-dependency——生成器把需求 id 写入草稿 `depends:`，而该字段要求 spec 名 | 删除全部草稿的悬空 depends 行（依赖真源在 KLL Requirements 节，spec 经 satisfies 追溯）；复跑 plan --gate 通过 |
